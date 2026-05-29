//! LLM-callable tool vocabulary. Tools are thin wrappers over
//! `crates/core`: argument validation, dispatch, session mutation,
//! and audit-log recording. No business logic lives here.
//!
//! - The closed [`Tool`] enum partitions into [`BatchableTool`] and
//!   [`HighImpactTool`] sealed sub-enums. The alone-in-turn predicate
//!   is structural (`matches!(tool, Tool::HighImpact(_))`) — a future
//!   variant cannot accidentally land in the wrong bucket without
//!   changing its variant type. [`Tool::is_mutation`] /
//!   [`Tool::is_alone_in_turn`] / [`Tool::name`] / [`Tool::spec`]
//!   stay in `mod.rs` — auditability invariant.
//! - [`dispatch_one`] / [`dispatch_batch`] also stay here, forming the
//!   boundary contract.
//! - Tool bodies live in per-domain submodules. Shared helpers
//!   (`rebuild_dag`, `workflow_id`, `validate_discover_stage`,
//!   `invalidate_and_rebuild`, `state_delta`) live here and are
//!   re-exported to submodules via `pub(super)`.

use crate::errors::{ToolError, ToolResult};
use crate::session::{Session, ToolCallRecord};
use chrono::Utc;
use ecaa_workflow_core::dag::TaskState;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;
use tracing;
use uuid::Uuid;

pub(crate) mod amend;
mod atoms;
pub mod branch;
mod classification;
mod compact;
pub(crate) mod conversational;
mod emit;
pub(crate) mod execution;
mod hypothesized_node;
mod hypothesized_renderer;
mod intake;
pub mod literature_context;
mod result;
mod sensitivity;
mod taxonomy;
pub(crate) mod violation_log;

pub(crate) use compact::{cap_tool_result_length, compact_tabular};

#[cfg(test)]
mod tests;

// Compatibility surface — `append_intake_prose` is invoked by the
// conversation service's `maybe_auto_append` helper outside of the
// tool-dispatch loop. Re-exported at the crate level so that caller
// stays unchanged.

pub(crate) use intake::append_intake_prose;

/// Direct-call entry point for the `propose_hypothesized_renderer` tool body,
/// used by the REST endpoint
/// `POST /api/chat/session/:id/tool/propose_hypothesized_renderer`.
///
/// This is a thin public(crate) forwarder that bypasses the full LLM
/// dispatch loop (`dispatch_one`) — callers (the server handler) load the
/// `PlotAffordanceRegistry` once and pass it in, avoiding the per-call
/// registry load inside `dispatch_inner`. The function signature mirrors
/// `hypothesized_renderer::propose_hypothesized_renderer` exactly.
pub fn hypothesized_renderer_dispatch(
    session: &mut Session,
    registry: &dyn ecaa_workflow_core::plot_affordance::PlotAffordanceRegistry,
    target_semantic_type: &str,
    proposed_parent_terms: &[String],
    proposed_figure_ids: &[String],
    sme_intent: &str,
    primitive_basis: Option<&str>,
) -> ToolResult {
    hypothesized_renderer::propose_hypothesized_renderer(
        session,
        registry,
        target_semantic_type,
        proposed_parent_terms,
        proposed_figure_ids,
        sme_intent,
        primitive_basis,
    )
}

/// Closed tool vocabulary, partitioned into two sealed bucket enums
/// (`BatchableTool` for read-only + intake mutation + conversational
/// tools, `HighImpactTool` for the 8 alone-in-turn high-impact tools).
/// The partition makes the alone-in-turn rule structurally enforced:
/// the dispatcher's batch entry point takes a `Vec<(Uuid, BatchableTool)>`
/// and the single-call entry point takes a `(Uuid, HighImpactTool)`, so
/// a mixed batch is a type error rather than a runtime rejection.
///
/// The on-wire JSON shape is preserved by `#[serde(untagged)]` here +
/// `#[serde(tag = "tool_name", rename_all = "snake_case")]` on each
/// sub-enum: `{"tool_name": "emit_package", ...}` round-trips through
/// either bucket regardless of which variant matches first.
///
/// `Tool::COUNT` is an inherent associated const summing the two
/// bucket counts (see `impl Tool`). No `strum::EnumCount` derive on
/// the outer enum — that would surface a misleading "count = 2"
/// through the trait method, even though the inherent const shadows it.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(untagged)]
pub enum Tool {
    /// Tools the LLM may freely batch in a single turn. Read-only,
    /// intake-mutation, and conversational-control tools all live here.
    Batchable(BatchableTool),
    /// Tools the dispatcher requires to be the sole call in their turn.
    /// CLAUDE.md pins this set at exactly 8 entries; the partition is
    /// load-bearing for the eval-runner's batched-high-impact-tool
    /// violation counter.
    HighImpact(HighImpactTool),
}

/// Read-only + intake-mutation + conversational tools. May be batched
/// freely within a single LLM turn; the dispatcher runs them in the
/// order received.
#[derive(Debug, Clone, Serialize, Deserialize, strum::EnumCount, schemars::JsonSchema)]
#[serde(tag = "tool_name", rename_all = "snake_case")]
#[non_exhaustive]
pub enum BatchableTool {
    // ── Read-only ────────────────────────────────────────────
    /// Run the deterministic classifier over new intake prose and update the session classification.
    ClassifyIntake {
        /// Free-text intake prose to classify.
        prose: String,
    },
    /// Return the taxonomy info (atom list, method options) for a given modality.
    GetTaxonomyInfo {
        /// Modality identifier (e.g. `bulk_rnaseq`).
        modality_id: String,
    },
    /// Return the current session state snapshot (state, intake, DAG summary).
    GetSessionState,
    /// Return the raw classifier evidence (keyword hits, confidence breakdown).
    GetClassificationEvidence,
    /// Fetch the structured result for a completed task — method
    /// summary, key-output artifact refs, metric bundle, completion
    /// timestamp. The LLM calls this when the user asks to see what a
    /// task produced.
    GetTaskResult {
        /// Identifier of the completed task to fetch results for.
        task_id: String,
    },
    /// Return PMID-anchored PriorClaimRow + ClaimsEvidenceRow rows for a
    /// named entity from the session's most recently emitted package.
    /// Read-only; reads from prior_claims_matrix.csv (upstream
    /// review_prior_work atom) and claims_evidence_matrix.csv (downstream
    /// contextualize_findings_with_literature atom). Sub-millisecond, no
    /// live PubMed call. Returns LiteratureContextError::NoLiteratureAtoms
    /// when neither CSV is present. Spec §10.1.
    GetLiteratureContext {
        /// Gene symbol or protein name to look up in the emitted package's literature CSVs.
        entity: String,
        /// Optional kind hint to narrow the search (gene, pathway, drug, etc.).
        #[serde(default)]
        entity_kind: Option<literature_context::EntityKind>,
    },
    /// Read-only catalog inspection. Returns a filtered summary of
    /// `config/stage-atoms/*.yaml` so the LLM can avoid duplicate
    /// `propose_hypothesized_node` calls (check if an atom already
    /// exists), pick valid `candidate_tools` for `amend_stage_method`,
    /// and answer SME questions about coverage ("which modalities have
    /// DTU atoms?"). Sub-millisecond on the ~73-atom catalog; the
    /// registry is reloaded per call (no global cache). All filters
    /// are AND'd together; an empty args object returns the first
    /// `DEFAULT_MAX_RESULTS` atoms.
    ListAtoms {
        /// Filter and pagination arguments for the atom catalog query.
        #[serde(default, flatten)]
        args: atoms::ListAtomsArgs,
    },

    // ── Intake mutation ──────────────────────────────────────
    /// Set a single structured field on a stage's intake record.
    SetIntakeField {
        /// Stage id the field belongs to.
        stage: String,
        /// Field name within that stage's intake schema.
        field: String,
        /// New value for the field.
        value: serde_json::Value,
    },
    /// Pin the method string for a stage (only fires when the SME named it unprompted).
    SetIntakeMethod {
        /// Stage id to set the method on.
        stage: String,
        /// Free-text method description captured from the SME.
        method_prose: String,
    },
    /// Append free-form prose to the running intake buffer and re-classify.
    AppendIntakeProse {
        /// Prose to append.
        prose: String,
    },
    /// Sub-archetype small-task scoping. Sets the SME's atom-exclusion
    /// list (count matrix → skip raw_qc/sequence_trimming/alignment/
    /// quantification; BAM/CRAM → skip raw_qc/sequence_trimming/
    /// alignment). The next rebuild_dag prunes every excluded atom +
    /// its discover_/validate_ companions; downstream atoms outside
    /// the exclusion set that lose all upstreams get REWIRED to
    /// data_acquisition (not cascade-dropped). Pass an empty atom_ids
    /// to clear the exclusion set.
    SetIntakeExcludedAtoms {
        /// Atom ids to exclude from the next DAG build; empty list clears the exclusion set.
        atom_ids: Vec<String>,
    },
    /// Override the classifier's modality choice. Used when the SME
    /// has explicitly disambiguated a tie via propose_quick_replies
    /// or otherwise confirmed a modality the classifier under-ranked.
    /// Rewrites session.classification.modality + archetype_id and
    /// triggers a rebuild_dag so the composer reseeds against the
    /// SME-confirmed modality.
    SetIntakeModality {
        /// Modality identifier to set (e.g. `bulk_rnaseq`).
        modality_id: String,
    },

    // ── Conversational control ───────────────────────────────
    /// Render the plan-summary card and advance the session to PendingConfirmation.
    ProposeSummaryConfirmation {
        /// Markdown summary text to present to the SME.
        summary_markdown: String,
    },
    /// Surface a set of quick-reply chips for the SME to choose from.
    ProposeQuickReplies {
        /// Question or prompt the SME is answering.
        question: String,
        /// Quick-reply chip labels.
        options: Vec<String>,
    },
}

/// High-impact tools that must be the only tool call in their turn.
/// The dispatcher's `dispatch_batch` rejects a batch carrying any
/// `HighImpactTool` alongside other calls. CLAUDE.md pins this set at
/// exactly 8 entries (`emit_package`, `amend_stage_method`,
/// `rerun_task`, `select_sensitivity_winner`, `branch_session`,
/// `start_execution`, `propose_hypothesized_node`,
/// `propose_hypothesized_renderer`).
#[derive(Debug, Clone, Serialize, Deserialize, strum::EnumCount, schemars::JsonSchema)]
#[serde(tag = "tool_name", rename_all = "snake_case")]
#[non_exhaustive]
pub enum HighImpactTool {
    /// Post-emission, swap the method recorded
    /// for a stage. Invalidates the forward slice of `stage` in the DAG,
    /// rebuilds that slice, and routes the session Emitted → Amending →
    /// ReadyToEmit. Alone-in-turn constraint enforced by dispatcher.
    AmendStageMethod {
        /// Stage id whose method is being replaced.
        stage: String,
        /// Replacement method description.
        method_prose: String,
        /// Required and non-empty when the target stage is
        /// prespecified in a confirmatory session; optional elsewhere.
        /// Carried through to the `PostHocDeviation` decision record.
        #[serde(default)]
        rationale: Option<String>,
    },
    /// The SME's sensitivity-comparison
    /// choice. Records the winning variant for a sensitivity_comparison
    /// stage, invalidates the downstream slice, and routes the session
    /// back to ReadyToEmit via the same transitions as
    /// `amend_stage_method`. Alone-in-turn.
    SelectSensitivityWinner {
        /// Sensitivity-comparison stage id.
        stage: String,
        /// The winning variant label (method or parameter set).
        winner: String,
        /// Optional SME-supplied rationale for the choice.
        #[serde(default)]
        rationale: Option<String>,
    },
    /// Rerun a previously-completed task
    /// with the same method. Thin wrapper on amend_stage_method with
    /// identical method_prose — semantics are "invalidate the
    /// downstream slice + rebuild the DAG + route back to
    /// ReadyToEmit". Used when the SME wants a fresh result from the
    /// same method (e.g., after input data drift). Alone-in-turn.
    RerunTask {
        /// Id of the previously-completed task to rerun.
        task_id: String,
        /// Optional SME-supplied reason for the rerun.
        #[serde(default)]
        reason: Option<String>,
    },
    /// Fork the current session
    /// into a new branched session that inherits intake state but
    /// has its own audit log + emit history. The dispatcher creates
    /// the branch via Session::branch_from and returns the new
    /// session id; the caller (server) handles persistence + routing.
    /// Alone-in-turn.
    BranchSession {
        /// Optional SME-supplied rationale for branching.
        #[serde(default)]
        rationale: Option<String>,
    },
    /// High-impact tool that compiles and emits the RO-Crate package.
    /// Requires `session.user_confirmed == true` set by the server-side
    /// `/confirm` endpoint before this tool fires.
    EmitPackage {
        /// Security audit C-10 / the LLM tool
        /// schema no longer exposes `output_dir`. The field is preserved
        /// in the enum shape so historical sessions deserialize, but
        /// any value the LLM tries to smuggle through is logged and
        /// then dropped at dispatch time — the resolver always uses
        /// the server-assigned path under `default_package_root()`.
        #[serde(default, deserialize_with = "ignore_llm_output_dir")]
        output_dir: Option<String>,
    },
    /// chat-driven execution kickoff. Fires only when the
    /// session is in Emitted state; the server's ExecutionStarter sink
    /// spawns the harness against the emitted package. Alone-in-turn.
    /// The SME triggers this by saying "yes, start execution" after
    /// emission; the LLM has no reason to fire it unprompted.
    ///
    /// Security audit C-3 / `agent_path` was
    /// removed from the tool schema. The harness-spawn path picks the
    /// production default (`scripts/agent-claude.sh`) and refuses any
    /// override that doesn't match the server-side allowlist
    /// (`crates/server/src/chat_routes/execution/start.rs::ALLOWED_AGENT_SCRIPTS`).
    /// The LLM never had a reason to choose an agent script; closing
    /// the field eliminates the RCE shape entirely.
    StartExecution {
        /// Optional cap on harness loop iterations for this run.
        #[serde(default)]
        max_iterations: Option<u32>,
    },

    /// Propose a
    /// hypothesized node when the SME asks for a capability the
    /// registry doesn't yet provide. Alone-in-turn (high-impact
    /// mutation tool). Arguments are a JSON-schema-validated
    /// HypothesizedNodeProposal carrying the proposed name +
    /// intent + ports + parent terms + assumptions + failure
    /// modes + minimal validation tests + LLM-summarized SME
    /// rationale. The handler appends the proposal to a session-
    /// scoped registry and writes a
    /// `DecisionType::ProposedHypothesizedNode` audit record on
    /// accept; rejections (LLM-recoverable schema errors) do not
    /// write to the decision log and are counted in
    /// side_call_cost_usd so over-eager proposals are observable.
    ///
    /// The tool body never mutates the executable DAG — only the
    /// `start_execution` / `emit_package` paths consult the
    /// proposals registry, and only after validation, sandbox, and
    /// promotion gates pass.
    ProposeHypothesizedNode {
        /// Proposed node id (snake_case; must not shadow a
        /// `ProductionNode` atom by name).
        proposed_id: String,
        /// One-sentence intent.
        intent: String,
        /// Proposed parent EDAM / local-extension term IRIs
        /// (e.g. `data:2603`).
        parent_terms: Vec<String>,
        /// Free-text SME justification (LLM-summarized from prior
        /// turns).
        llm_rationale: String,
        /// Declared assumptions (unresolved; SME confirms before
        /// promotion).
        #[serde(default, deserialize_with = "vec_string_or_null")]
        assumptions: Vec<String>,
        /// Declared failure modes the validators must cover.
        #[serde(default, deserialize_with = "vec_string_or_null")]
        failure_modes: Vec<String>,
        /// Minimal validation test ids the obligation registry
        /// must run (e.g. `p_value_in_unit_interval`).
        #[serde(default, deserialize_with = "vec_string_or_null")]
        validation_tests: Vec<String>,
        /// Atom-ids the proposed node depends on (becomes
        /// AtomDefinition.depends_on on promotion so the emitted DAG
        /// has at least one upstream edge for the new node).
        #[serde(default, deserialize_with = "vec_string_or_null")]
        upstream_atom_ids: Vec<String>,
    },

    /// Flexible plotting upgrade plan propose a preferred
    /// renderer when a figure resolved via `StructuralFallback`. Alone-
    /// in-turn (high-impact mutation tool). Mirrors
    /// `ProposeHypothesizedNode` but targets
    /// the PlotAffordanceRegistry instead of the DAG atom registry.
    ///
    /// The handler validates that every `proposed_parent_terms` entry
    /// resolves in the `PlotAffordanceRegistry`, that no
    /// `proposed_figure_ids` entry shadows a registered figure id, and
    /// that the `target_semantic_type` does not use the reserved `EDAM:`
    /// namespace. On accept it appends to `Session::renderer_proposals`
    /// and writes a `DecisionType::ProposedHypothesizedRenderer` audit
    /// record. Rejections do not write to the decision log.
    ///
    /// The tool body never mutates the closed `PlotAffordanceRegistry` —
    /// only the affordance resolver and catalog-promotion gates consult
    /// the renderer-proposals registry after promotion evidence accumulates.
    ProposeHypothesizedRenderer {
        /// SemanticType IRI of the output port the preferred renderer
        /// addresses (e.g. `ecaax:my_custom_output`). The `EDAM:` namespace
        /// is reserved for ontology-controlled terms and is rejected.
        target_semantic_type: String,
        /// Registered parent-term SemanticType IRIs the proposed renderer
        /// inherits from. Each must resolve via `registry.lookup_exact`.
        proposed_parent_terms: Vec<String>,
        /// Figure ids the preferred renderer would produce. None may
        /// shadow an existing registered figure id.
        proposed_figure_ids: Vec<String>,
        /// LLM-summarized SME description of the preferred renderer,
        /// ≤ 800 chars.
        sme_intent: String,
        /// The structural primitive id the SME is upgrading from, if any
        /// (e.g. `__structural_matrix_overview`). Omit when the fallback
        /// primitive is unknown.
        #[serde(default)]
        primitive_basis: Option<String>,
    },
}

impl From<BatchableTool> for Tool {
    fn from(t: BatchableTool) -> Self {
        Tool::Batchable(t)
    }
}

impl From<HighImpactTool> for Tool {
    fn from(t: HighImpactTool) -> Self {
        Tool::HighImpact(t)
    }
}

/// Per-tool metadata. One row per [`Tool`] variant; every predicate the
/// dispatcher uses lives in this struct so adding a new policy column
/// (e.g. `requires_network`, `requires_confirmation`) is editing one
/// table instead of growing a fourth match arm.
///
/// The former `is_alone_in_turn: bool` field was removed when [`Tool`]
/// was split into [`BatchableTool`] / [`HighImpactTool`] sealed enums:
/// the predicate is now structural (`matches!(tool, Tool::HighImpact(_))`)
/// rather than a per-row boolean that a future variant could forget to set.
pub struct ToolSpec {
    /// Canonical snake_case tool name (matches the `tool_name` serde tag and `Tool::name()`).
    pub name: &'static str,
    /// True when this tool writes session state (triggers `last_activity` bump on success).
    pub is_mutation: bool,
    /// Pre-handler state-machine trigger. When `Some`,
    /// the dispatcher consults `Session::can_transition` before invoking
    /// the handler; rejects with `PreconditionFailure` if the trigger
    /// is illegal from the current state, otherwise fires the
    /// transition so the handler runs in the new state. Handlers stop
    /// calling `try_transition` — the spec table is the single
    /// auditability surface for the closed-vocab × state-machine
    /// contract pair.
    pub state_trigger: Option<StateTriggerSpec>,
    /// Post-handler state-machine hooks. When `Some`,
    /// the dispatcher fires `on_ok` after a successful handler return
    /// or `on_err` after an error. Both receive `&mut Session`; the
    /// `on_err` variant additionally sees the `ToolError` so triggers
    /// like `EmitPackageErr` can carry the reason. Used by handlers
    /// whose post-state depends on Result (`emit_package`,
    /// `amend_stage_method`).
    pub post_handler: Option<PostHandlerSpec>,
}

/// Pre-handler trigger choice.
///
/// * `Static` — fire this trigger pre-handler and reject the call
///   with `PreconditionFailure` if the transition is illegal from the
///   current state. Used by `emit_package` (the dispatcher must
///   advance ReadyToEmit → Emitting before the handler writes the
///   package).
/// * `Dynamic` — same as `Static` but the trigger is computed from
///   session state.
/// * `TolerantStatic` — fire pre-handler, fire-and-forget. An illegal
///   transition is silently ignored (matches the legacy
///   `let _ = session.try_transition(...)` discipline). Used by
///   `append_intake_prose` (fires `Greeting → Intake` so the
///   subsequent in-handler `rebuild_dag` sees the right state to fire
///   `DagBuiltWithUnresolvedDiscovery` itself), and by
///   `propose_summary_confirmation` (advance Intake →
///   PendingConfirmation; harmless from terminal states).
pub enum StateTriggerSpec {
    /// Fire the trigger before the handler; reject with `PreconditionFailure` on illegal transitions.
    Static(crate::session::StateTrigger),
    /// Like `Static` but computes the trigger dynamically from session state.
    Dynamic(fn(&Session) -> Option<crate::session::StateTrigger>),
    /// Fire the trigger before the handler; silently ignore illegal transitions.
    TolerantStatic(crate::session::StateTrigger),
}

/// Post-handler hooks. Each callback fires from
/// `dispatch_one` after the handler returns; both run in `mod.rs` so
/// the regression test `state_machine_centralization::all_handlers_state_clean`
/// can grep `tools/<handler>.rs` for residual `try_transition(` calls
/// without false-positives on the spec-table closures.
pub struct PostHandlerSpec {
    /// Fired after a successful handler return (`ToolResult::is_error == false`).
    pub on_ok: Option<fn(&mut Session)>,
    /// Fired after an error handler return; receives the `ToolError` for state-specific handling.
    pub on_err: Option<fn(&mut Session, &ToolError)>,
}

// Post-handler closures. Co-located with the spec table
// so the `state_machine_centralization::all_handlers_state_clean` test
// can grep handler files for residual `try_transition(` without
// flagging the centralized spec-table closures.
//
// All mutating handlers that need to drive a post-handler transition
// push their triggers onto `session.deferred_state_triggers`; this
// drain function fires them in order and clears the queue. Lives in
// `mod.rs` (not in any handler file) so the regression-test grep stays
// clean.

fn drain_deferred_state_triggers_post_ok(session: &mut Session) {
    // R3.6 — atomic drain. Handlers like `amend_stage_method` queue a
    // PAIR of triggers (AmendStart → Amending, then AmendReady →
    // ReadyToEmit) that only make sense applied together. If the
    // second trigger is illegal from the intermediate state the
    // session would stick in `Amending` forever with no UI affordance
    // to recover.
    //
    // Atomicity strategy: snapshot the prior state, attempt every
    // trigger in order, and on the first failure restore the prior
    // state + push the unfired remainder back onto the queue so a
    // follow-up turn can retry. Successful intermediate transitions
    // are rolled back along with the failed one — partial application
    // is never persisted to disk because `update`/`transaction` only
    // serialize after this drain returns.
    let triggers = std::mem::take(&mut session.deferred_state_triggers);
    if triggers.is_empty() {
        return;
    }
    let prior_state = session.state.clone();
    for (idx, t) in triggers.iter().enumerate() {
        if let Err(err) = session.try_transition(t.clone()) {
            warn_illegal_transition(session, t, &err);
            // Roll back: restore the pre-drain state and re-queue the
            // remainder (including the failing trigger). The handler's
            // mutation work has already been applied; only the state-machine
            // transitions are reverted.
            session.state = prior_state;
            session.deferred_state_triggers = triggers[idx..].to_vec();
            return;
        }
    }
}

fn warn_illegal_transition(
    session: &Session,
    trigger: &crate::session::StateTrigger,
    err: &crate::session::TransitionError,
) {
    tracing::warn!(
        session_id = %session.id,
        trigger = ?trigger,
        current_state = ?session.state,
        error = ?err,
        "illegal state transition ignored"
    );
}

fn drain_deferred_state_triggers_post_err(session: &mut Session, _err: &ToolError) {
    // On error, we still drain the queue (clear it) so the next
    // handler doesn't inherit triggers from a failed call. We DO NOT
    // fire the deferred triggers on error — handlers push triggers
    // assuming a successful path. The `emit_package` error case has
    // its own dedicated `EmitPackageErr` post-err hook below.
    session.deferred_state_triggers.clear();
}

fn emit_package_post_ok(session: &mut Session) {
    // Drain any deferred triggers first (defensive — emit_package
    // handlers don't push today, but a future change might).
    drain_deferred_state_triggers_post_ok(session);
    // Single-use latch: consume the token so a replay of emit_package
    // with the same (emission_id, summary_hash) fails the precondition.
    // The SME must click Confirm again to mint a fresh token before
    // re-emitting (e.g. after amend or a failed-then-retried emit).
    if let Some(token) = session.confirmation_token.as_mut() {
        token.consume();
    }
    let trigger = crate::session::StateTrigger::EmitPackageOk;
    if let Err(err) = session.try_transition(trigger.clone()) {
        warn_illegal_transition(session, &trigger, &err);
    }
}

fn emit_package_post_err(session: &mut Session, err: &ToolError) {
    // Clear any pushed-but-unfired deferred triggers since the emit
    // failed; we don't want a partial chain firing.
    session.deferred_state_triggers.clear();
    let trigger = crate::session::StateTrigger::EmitPackageErr {
        reason: err.short_reason(),
    };
    if let Err(transition_err) = session.try_transition(trigger.clone()) {
        warn_illegal_transition(session, &trigger, &transition_err);
    }
}

// One spec const per Tool variant. Reordered to mirror the enum
// declaration so the column structure is scannable.
const SPEC_CLASSIFY_INTAKE: ToolSpec = ToolSpec {
    name: "classify_intake",
    is_mutation: false,
    state_trigger: None,
    post_handler: None,
};
const SPEC_GET_TAXONOMY_INFO: ToolSpec = ToolSpec {
    name: "get_taxonomy_info",
    is_mutation: false,
    state_trigger: None,
    post_handler: None,
};
const SPEC_LIST_ATOMS: ToolSpec = ToolSpec {
    name: "list_atoms",
    is_mutation: false,
    state_trigger: None,
    post_handler: None,
};
const SPEC_GET_SESSION_STATE: ToolSpec = ToolSpec {
    name: "get_session_state",
    is_mutation: false,
    state_trigger: None,
    post_handler: None,
};
const SPEC_GET_CLASSIFICATION_EVIDENCE: ToolSpec = ToolSpec {
    name: "get_classification_evidence",
    is_mutation: false,
    state_trigger: None,
    post_handler: None,
};
const SPEC_GET_TASK_RESULT: ToolSpec = ToolSpec {
    name: "get_task_result",
    is_mutation: false,
    state_trigger: None,
    post_handler: None,
};
const SPEC_GET_LITERATURE_CONTEXT: ToolSpec = ToolSpec {
    name: "get_literature_context",
    is_mutation: false,
    state_trigger: None,
    post_handler: None,
};
const SPEC_SET_INTAKE_FIELD: ToolSpec = ToolSpec {
    name: "set_intake_field",
    is_mutation: true,
    state_trigger: None,
    post_handler: None,
};
const SPEC_SET_INTAKE_METHOD: ToolSpec = ToolSpec {
    name: "set_intake_method",
    is_mutation: true,
    state_trigger: None,
    post_handler: None,
};
const SPEC_APPEND_INTAKE_PROSE: ToolSpec = ToolSpec {
    name: "append_intake_prose",
    is_mutation: true,
    // TolerantStatic so the trigger fires Greeting → Intake BEFORE the
    // handler's `rebuild_dag` runs (which itself needs Intake state to
    // fire `DagBuiltWithUnresolvedDiscovery` correctly into
    // IntakeFollowup). From Emitted (and other terminal states) the
    // trigger is illegal; the Tolerant variant swallows the error
    // silently — matching the legacy fire-and-forget discipline.
    state_trigger: Some(StateTriggerSpec::TolerantStatic(
        crate::session::StateTrigger::AppendProse,
    )),
    post_handler: None,
};
const SPEC_SET_INTAKE_EXCLUDED_ATOMS: ToolSpec = ToolSpec {
    name: "set_intake_excluded_atoms",
    is_mutation: true,
    state_trigger: None,
    post_handler: None,
};
const SPEC_SET_INTAKE_MODALITY: ToolSpec = ToolSpec {
    name: "set_intake_modality",
    is_mutation: true,
    state_trigger: None,
    post_handler: None,
};
const SPEC_AMEND_STAGE_METHOD: ToolSpec = ToolSpec {
    name: "amend_stage_method",
    is_mutation: true,
    state_trigger: None,
    // The handler pushes `AmendStart { target_stage, invalidated_tasks }`
    // and `AmendReady` onto `session.deferred_state_triggers`; the
    // drain fires them in order after the handler returns Ok.
    post_handler: Some(PostHandlerSpec {
        on_ok: Some(drain_deferred_state_triggers_post_ok),
        on_err: Some(drain_deferred_state_triggers_post_err),
    }),
};
const SPEC_SELECT_SENSITIVITY_WINNER: ToolSpec = ToolSpec {
    name: "select_sensitivity_winner",
    is_mutation: true,
    state_trigger: None,
    // Handler validates the winner against `Blocked.AwaitingSmeSelection`
    // candidates while still in Blocked, then pushes
    // `SensitivityWinnerSelected` onto `deferred_state_triggers`. The
    // drain fires Blocked → Intake AFTER the handler's
    // `invalidate_and_rebuild` has already run (which is fine — that
    // rebuild's internal `DagBuiltWithUnresolvedDiscovery` from
    // Blocked is a tolerated no-op).
    post_handler: Some(PostHandlerSpec {
        on_ok: Some(drain_deferred_state_triggers_post_ok),
        on_err: Some(drain_deferred_state_triggers_post_err),
    }),
};
const SPEC_RERUN_TASK: ToolSpec = ToolSpec {
    name: "rerun_task",
    is_mutation: true,
    state_trigger: None,
    // `rerun_task` delegates to `amend_stage_method` internally, which
    // pushes its own deferred triggers — drain them via the same hook
    // so the rerun → amend pathway carries the state transitions
    // through.
    post_handler: Some(PostHandlerSpec {
        on_ok: Some(drain_deferred_state_triggers_post_ok),
        on_err: Some(drain_deferred_state_triggers_post_err),
    }),
};
const SPEC_BRANCH_SESSION: ToolSpec = ToolSpec {
    name: "branch_session",
    is_mutation: true,
    state_trigger: None,
    post_handler: None,
};
const SPEC_EMIT_PACKAGE: ToolSpec = ToolSpec {
    name: "emit_package",
    is_mutation: true,
    state_trigger: Some(StateTriggerSpec::Static(
        crate::session::StateTrigger::EmitPackageStart,
    )),
    post_handler: Some(PostHandlerSpec {
        on_ok: Some(emit_package_post_ok),
        on_err: Some(emit_package_post_err),
    }),
};
const SPEC_START_EXECUTION: ToolSpec = ToolSpec {
    name: "start_execution",
    is_mutation: true,
    state_trigger: None,
    post_handler: None,
};
const SPEC_PROPOSE_SUMMARY_CONFIRMATION: ToolSpec = ToolSpec {
    name: "propose_summary_confirmation",
    is_mutation: true,
    // TolerantStatic so the dispatcher advances Intake →
    // PendingConfirmation before the handler renders the summary card.
    // From terminal states (Emitted) the trigger is illegal; the
    // Tolerant variant swallows the error silently — matching the
    // legacy `is_err()` short-circuit-then-PreconditionFailure path,
    // except now the card still renders (the handler's own emptiness
    // check is the only hard precondition).
    state_trigger: Some(StateTriggerSpec::TolerantStatic(
        crate::session::StateTrigger::ProposeSummaryConfirmation,
    )),
    post_handler: None,
};
const SPEC_PROPOSE_QUICK_REPLIES: ToolSpec = ToolSpec {
    name: "propose_quick_replies",
    is_mutation: false,
    state_trigger: None,
    post_handler: None,
};
const SPEC_PROPOSE_HYPOTHESIZED_NODE: ToolSpec = ToolSpec {
    name: "propose_hypothesized_node",
    is_mutation: true,
    state_trigger: None,
    post_handler: None,
};
const SPEC_PROPOSE_HYPOTHESIZED_RENDERER: ToolSpec = ToolSpec {
    name: "propose_hypothesized_renderer",
    is_mutation: true,
    state_trigger: None,
    post_handler: None,
};

/// Tolerate JSON `null` for optional `Vec<String>` tool-call fields.
/// The Anthropic Messages API JSON schema marks these as optional, but
/// some prompts surface them as `null` rather than omitting the key;
/// vanilla `#[serde(default)]` rejects null on a Vec because the
/// default deserializer for `Vec<String>` is an array, not nullable.
/// Without this adapter, an LLM tool-call like
/// `{"proposed_id":"x", "assumptions": null, ...}`
/// fails with "deserializing tool_use payload for 'propose_hypothesized_node'",
/// aborts the tool loop, and surfaces a backend_error to the SME.
/// Applied to the optional Vec<String> fields on `ProposeHypothesizedNode`
/// and sibling tool-call variants where missing-or-empty semantics are
/// the intent.
pub(crate) fn vec_string_or_null<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::Deserialize;
    let opt: Option<Vec<String>> = Option::deserialize(deserializer)?;
    Ok(opt.unwrap_or_default())
}

/// Security audit C-10: the LLM tool schema no longer
/// advertises `output_dir`, but a hostile / fine-tuned model could
/// still hand-craft a tool-call JSON that includes the property. This
/// `deserialize_with` adapter receives whatever the wire payload
/// contains, logs it, and then yields `None` so the dispatcher cannot
/// observe attacker bytes. The CLI/test entry points construct the
/// variant directly (not via deserialization) and bypass this path.
fn ignore_llm_output_dir<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let observed: Option<String> = Option::deserialize(deserializer)?;
    if let Some(v) = observed.as_deref() {
        if !v.trim().is_empty() {
            tracing::warn!(
                value = %v,
                "dropping LLM-supplied emit_package output_dir; server-controlled path will be used \
                 (C-10 hardening)"
            );
        }
    }
    Ok(None)
}

impl Tool {
    /// Look up the per-variant policy row. The match keeps compile-time
    /// exhaustiveness — a new [`Tool`] variant added without a row here
    /// is a build error, not a runtime surprise.
    ///
    /// The `non_exhaustive_omitted_patterns` lint
    /// surfaces a missed variant at lint level so partial PRs that
    /// touch the dispatch surface fail in review without needing a
    /// full clippy gate. The lint is gated on the unstable feature of
    /// the same name (rust-lang/rust#89554); on stable toolchains it's
    /// a no-op (unknown_lints suppressed) — the runtime
    /// `tool_schemas_consistency::names_match_enum` cross-check still
    /// guards drift. When the lint stabilises this attribute starts
    /// firing automatically.
    pub fn spec(&self) -> &'static ToolSpec {
        match self {
            Tool::Batchable(b) => b.spec(),
            Tool::HighImpact(h) => h.spec(),
        }
    }

    /// Return the canonical snake_case name for this tool variant.
    pub fn name(&self) -> &'static str {
        self.spec().name
    }

    /// Return whether this tool writes session state.
    pub fn is_mutation(&self) -> bool {
        self.spec().is_mutation
    }

    /// True when this tool must be the only call in its assistant turn.
    /// After the [`BatchableTool`]/[`HighImpactTool`] split this is a
    /// structural predicate — no per-row table lookup, no risk of a
    /// future variant forgetting to flip the flag. The runtime check
    /// inside `dispatch_batch` remains as a sanity guard and an audit
    /// surface for the eval runner's batched-high-impact-tool counter.
    pub fn is_alone_in_turn(&self) -> bool {
        matches!(self, Tool::HighImpact(_))
    }

    /// Variant count across both bucket enums. Anchored compile-time
    /// via `BatchableTool::COUNT + HighImpactTool::COUNT` so adding a
    /// variant to either bucket updates this constant without manual
    /// bookkeeping. CLAUDE.md pins the documented total to this value.
    pub const COUNT: usize =
        <BatchableTool as strum::EnumCount>::COUNT + <HighImpactTool as strum::EnumCount>::COUNT;

    /// Return one sample value per [`Tool`] variant across both buckets.
    /// Used by the `tool_schemas_consistency::names_match_enum` cross-check
    /// (so the `Tool` enum and `tool_schemas()` JSON list cannot drift)
    /// and by the existing exhaustive-match smoke tests in
    /// `tool_schemas.rs` and `tools/tests.rs`. The matches inside
    /// `BatchableTool::all_variants_for_tests` and
    /// `HighImpactTool::all_variants_for_tests` stay exhaustive — adding
    /// a variant to either bucket without extending the corresponding
    /// list is a compile error.
    pub fn all_variants_for_tests() -> Vec<Tool> {
        let mut out: Vec<Tool> = BatchableTool::all_variants_for_tests()
            .into_iter()
            .map(Tool::Batchable)
            .collect();
        out.extend(
            HighImpactTool::all_variants_for_tests()
                .into_iter()
                .map(Tool::HighImpact),
        );
        out
    }
}

// ── Bucket-enum impls: spec lookup + sample-variant fixtures ───────────────

impl BatchableTool {
    /// Return the policy row for this variant.
    pub fn spec(&self) -> &'static ToolSpec {
        #[allow(unknown_lints)]
        #[deny(non_exhaustive_omitted_patterns)]
        match self {
            BatchableTool::ClassifyIntake { .. } => &SPEC_CLASSIFY_INTAKE,
            BatchableTool::GetTaxonomyInfo { .. } => &SPEC_GET_TAXONOMY_INFO,
            BatchableTool::GetSessionState => &SPEC_GET_SESSION_STATE,
            BatchableTool::GetClassificationEvidence => &SPEC_GET_CLASSIFICATION_EVIDENCE,
            BatchableTool::GetTaskResult { .. } => &SPEC_GET_TASK_RESULT,
            BatchableTool::GetLiteratureContext { .. } => &SPEC_GET_LITERATURE_CONTEXT,
            BatchableTool::ListAtoms { .. } => &SPEC_LIST_ATOMS,
            BatchableTool::SetIntakeField { .. } => &SPEC_SET_INTAKE_FIELD,
            BatchableTool::SetIntakeMethod { .. } => &SPEC_SET_INTAKE_METHOD,
            BatchableTool::AppendIntakeProse { .. } => &SPEC_APPEND_INTAKE_PROSE,
            BatchableTool::SetIntakeExcludedAtoms { .. } => &SPEC_SET_INTAKE_EXCLUDED_ATOMS,
            BatchableTool::SetIntakeModality { .. } => &SPEC_SET_INTAKE_MODALITY,
            BatchableTool::ProposeSummaryConfirmation { .. } => &SPEC_PROPOSE_SUMMARY_CONFIRMATION,
            BatchableTool::ProposeQuickReplies { .. } => &SPEC_PROPOSE_QUICK_REPLIES,
        }
    }

    /// Return the canonical snake_case name for this tool variant.
    pub fn name(&self) -> &'static str {
        self.spec().name
    }

    /// Return whether this tool writes session state.
    pub fn is_mutation(&self) -> bool {
        self.spec().is_mutation
    }

    /// Return one representative sample value per variant, used by exhaustiveness tests.
    pub fn all_variants_for_tests() -> Vec<BatchableTool> {
        vec![
            BatchableTool::ClassifyIntake { prose: "x".into() },
            BatchableTool::GetTaxonomyInfo {
                modality_id: "x".into(),
            },
            BatchableTool::GetSessionState,
            BatchableTool::GetClassificationEvidence,
            BatchableTool::GetTaskResult {
                task_id: "x".into(),
            },
            BatchableTool::GetLiteratureContext {
                entity: "ACAN".into(),
                entity_kind: None,
            },
            BatchableTool::ListAtoms {
                args: atoms::ListAtomsArgs {
                    modality: None,
                    role: None,
                    has_method_choice: None,
                    produces_edam_data: None,
                    max_results: None,
                },
            },
            BatchableTool::SetIntakeField {
                stage: "x".into(),
                field: "y".into(),
                value: serde_json::Value::Null,
            },
            BatchableTool::SetIntakeMethod {
                stage: "x".into(),
                method_prose: "y".into(),
            },
            BatchableTool::AppendIntakeProse { prose: "x".into() },
            BatchableTool::SetIntakeExcludedAtoms { atom_ids: vec![] },
            BatchableTool::SetIntakeModality {
                modality_id: "bulk_rnaseq".into(),
            },
            BatchableTool::ProposeSummaryConfirmation {
                summary_markdown: "x".into(),
            },
            BatchableTool::ProposeQuickReplies {
                question: "x".into(),
                options: vec!["y".into()],
            },
        ]
    }
}

impl HighImpactTool {
    /// Return the policy row for this variant.
    pub fn spec(&self) -> &'static ToolSpec {
        #[allow(unknown_lints)]
        #[deny(non_exhaustive_omitted_patterns)]
        match self {
            HighImpactTool::AmendStageMethod { .. } => &SPEC_AMEND_STAGE_METHOD,
            HighImpactTool::SelectSensitivityWinner { .. } => &SPEC_SELECT_SENSITIVITY_WINNER,
            HighImpactTool::RerunTask { .. } => &SPEC_RERUN_TASK,
            HighImpactTool::BranchSession { .. } => &SPEC_BRANCH_SESSION,
            HighImpactTool::EmitPackage { .. } => &SPEC_EMIT_PACKAGE,
            HighImpactTool::StartExecution { .. } => &SPEC_START_EXECUTION,
            HighImpactTool::ProposeHypothesizedNode { .. } => &SPEC_PROPOSE_HYPOTHESIZED_NODE,
            HighImpactTool::ProposeHypothesizedRenderer { .. } => {
                &SPEC_PROPOSE_HYPOTHESIZED_RENDERER
            }
        }
    }

    /// Return the canonical snake_case name for this tool variant.
    pub fn name(&self) -> &'static str {
        self.spec().name
    }

    /// Return whether this tool writes session state.
    pub fn is_mutation(&self) -> bool {
        self.spec().is_mutation
    }

    /// Return one representative sample value per variant, used by exhaustiveness tests.
    pub fn all_variants_for_tests() -> Vec<HighImpactTool> {
        vec![
            HighImpactTool::AmendStageMethod {
                stage: "x".into(),
                method_prose: "y".into(),
                rationale: None,
            },
            HighImpactTool::SelectSensitivityWinner {
                stage: "x".into(),
                winner: "y".into(),
                rationale: None,
            },
            HighImpactTool::RerunTask {
                task_id: "x".into(),
                reason: None,
            },
            HighImpactTool::BranchSession { rationale: None },
            HighImpactTool::EmitPackage { output_dir: None },
            HighImpactTool::StartExecution {
                max_iterations: None,
            },
            HighImpactTool::ProposeHypothesizedNode {
                proposed_id: "x".into(),
                intent: "x".into(),
                parent_terms: vec!["data:2603".into()],
                llm_rationale: "x".into(),
                assumptions: vec![],
                failure_modes: vec![],
                validation_tests: vec![],
                upstream_atom_ids: vec![],
            },
            HighImpactTool::ProposeHypothesizedRenderer {
                target_semantic_type: "ecaax:test_type".into(),
                proposed_parent_terms: vec!["EDAM:data_3134".into()],
                proposed_figure_ids: vec!["test_plot".into()],
                sme_intent: "test intent".into(),
                primitive_basis: None,
            },
        ]
    }
}

/// Runtime context the dispatcher needs but is not part of the LLM-visible
/// tool args — config root, optional Anthropic model id for audit, etc.
#[derive(Clone)]
pub struct ToolContext {
    /// Path to the `config/` directory for taxonomy + policy loading.
    pub config_dir: PathBuf,
    /// Serialized `ModelId` of the model that produced the current turn.
    pub model: String,
    /// the event sink the StartExecution tool calls to
    /// ask the server to spawn the harness. None in CLI / test paths
    /// (the tool still succeeds; the server-spawn side is a no-op).
    pub event_sink: Option<Arc<dyn crate::service::ServiceEventSink>>,
    /// Session id passed through so the StartExecution handler can
    /// route the request to the right session. ToolContext was
    /// previously session-agnostic; this is the one tool that needs
    /// to know which session triggered it.
    pub session_id: Option<crate::SessionId>,
    /// Optional handle on the
    /// MetricsStore so `emit_package` can append a row to
    /// `<pkg>/runtime/cost-ledger.jsonl` after a successful emit. None
    /// in unit-test / CLI paths; populated by the service loop's
    /// `with_metrics` builder so a real chat session writes the ledger
    /// every time `emit_package` succeeds. `MetricsStore` is internally
    /// `Arc<RwLock<...>>` so cloning by value here is cheap and shares
    /// the same backing state.
    pub metrics_store: Option<crate::metrics::MetricsStore>,
    /// Optional handle on
    /// the SessionStore so high-impact tools (currently `emit_package`)
    /// can re-read the persisted `user_confirmed` flag + pending
    /// `proposals` at execution time rather than trusting the tool
    /// loop's local clone, which may be seconds-to-minutes stale.
    /// Closes the gate-skew race where a concurrent /confirm or
    /// /proposals/:id/approve lands between the loop's snapshot and
    /// the emit dispatch. None in unit-test / CLI paths.
    pub store: Option<crate::persistence::SessionStore>,
}

impl std::fmt::Debug for ToolContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolContext")
            .field("config_dir", &self.config_dir)
            .field("model", &self.model)
            .field("event_sink", &self.event_sink.as_ref().map(|_| "<sink>"))
            .field("session_id", &self.session_id)
            .field(
                "metrics_store",
                &self.metrics_store.as_ref().map(|_| "<store>"),
            )
            .finish()
    }
}

impl ToolContext {
    /// Construct a minimal `ToolContext` for CLI/test paths (no sink, no session id, no stores).
    pub fn new(config_dir: impl Into<PathBuf>, model: impl Into<String>) -> Self {
        Self {
            config_dir: config_dir.into(),
            model: model.into(),
            event_sink: None,
            session_id: None,
            metrics_store: None,
            store: None,
        }
    }

    /// Builder-style setter used by the service loop to thread the SSE
    /// event sink + session id through to StartExecution.
    pub fn with_session_sink(
        mut self,
        session_id: crate::SessionId,
        sink: Option<Arc<dyn crate::service::ServiceEventSink>>,
    ) -> Self {
        self.session_id = Some(session_id);
        self.event_sink = sink;
        self
    }

    /// Attach a MetricsStore handle so `emit_package` can
    /// write a cost-ledger row on success. Unit-test paths omit this
    /// and the ledger write is skipped silently.
    pub fn with_metrics(mut self, store: crate::metrics::MetricsStore) -> Self {
        self.metrics_store = Some(store);
        self
    }

    /// Attach a SessionStore so `emit_package` (and any future
    /// high-impact tool) can re-read fresh persisted state at execution
    /// time instead of trusting the tool loop's stale local snapshot.
    ///
    /// R3.9 — runtime guard against re-entrancy inside
    /// `SessionStore::transaction`. The per-session `tokio::sync::Mutex`
    /// is NOT re-entrant: if a tool dispatched inside `transaction()`
    /// were given a `ToolContext` with `store` populated, a handler
    /// that called `store.get(id)` or `store.update(id, ...)` would
    /// deadlock until the upstream 300s timeout fires (this exact
    /// regression burned the auto-emit path until commit `64f690d9`).
    ///
    /// The phantom-typed surface (`ToolContext<InsideTransaction>` /
    /// `ToolContext<OutsideTransaction>`) would catch this at compile
    /// time but generic-ify ~65 call sites; instead we set a
    /// thread-local flag in `SessionStore::transaction` and assert
    /// here in debug builds. Tripping the assertion surfaces the
    /// re-entrancy at the wiring site, not at the deadlocked tool
    /// call site.
    pub fn with_store(mut self, store: crate::persistence::SessionStore) -> Self {
        debug_assert!(
            !crate::persistence::in_transaction(),
            "ToolContext::with_store called inside SessionStore::transaction \
             — tokio Mutex is not re-entrant; this will deadlock. \
             See R3.9 guard rationale in tools/mod.rs."
        );
        self.store = Some(store);
        self
    }
}

/// Dispatch a batch of tool calls for one assistant turn.
///
/// - Read-only tools execute concurrently via `join_all`.
/// - Mutation tools execute sequentially in the order given.
/// - `EmitPackage` and `AmendStageMethod` are alone-in-turn; if either
///   appears alongside another tool in the same batch, the entire batch
///   is rejected.
#[tracing::instrument(
    skip(session, ctx, tools),
    fields(batch_size = tools.len())
)]
pub async fn dispatch_batch(
    tools: Vec<(Uuid, Tool)>,
    session: &mut Session,
    ctx: &ToolContext,
) -> Vec<(Uuid, ToolResult)> {
    // Alone-in-turn enforcement for high-impact mutations.
    let alone_offender = tools
        .iter()
        .find(|(_, t)| t.is_alone_in_turn())
        .map(|(_, t)| t.name().to_string());
    if let Some(ref name) = alone_offender {
        if tools.len() > 1 {
            // Tier 17.1 / 17.2 — log the batched-high-impact-tool violation
            // before rejecting so the eval runner can count it.
            let runtime_root = violation_runtime_root(session);
            violation_log::emit(
                &violation_log::VocabularyViolation {
                    session_id: session.id.to_string(),
                    timestamp_ms: violation_log::now_ms(),
                    kind: violation_log::ViolationKind::BatchedHighImpactTools,
                    model: ctx.model.clone(),
                    tool_name: Some(name.clone()),
                    detail: format!(
                        "{} batched with {} other tool(s) in the same turn",
                        name,
                        tools.len() - 1
                    ),
                },
                &runtime_root,
            );
            let err = ToolError::PreconditionFailure {
                reason: format!("{} must be the only tool call in its turn", name),
                hint: format!(
                    "Split {} into its own assistant turn after the others complete.",
                    name
                ),
            };
            return tools
                .into_iter()
                .map(|(id, _)| (id, ToolResult::err(err.clone())))
                .collect();
        }
    }

    // Read-only batch first (parallel-eligible). With the current tool surface
    // every read tool is already a synchronous in-process call, so we just
    // drive them in declaration order — the LLM surface still allows real
    // parallel dispatch in the future without an API change.
    let mut results: Vec<(Uuid, ToolResult)> = Vec::with_capacity(tools.len());
    for (id, tool) in tools {
        let is_mut = tool.is_mutation();
        let res = dispatch_one(&tool, session, ctx).await;

        // Record audit trail for every dispatch (success or error)
        let record = ToolCallRecord {
            turn_id: id,
            tool_name: tool.name().to_string(),
            args: serde_json::to_value(&tool).unwrap_or(serde_json::Value::Null),
            result: res.content.clone(),
            is_error: res.is_error,
            model: ctx.model.clone(),
            timestamp: Utc::now(),
        };
        // cap the in-memory audit log at TOOL_CALL_LOG_CAP;
        // overflow rotates to `runtime/tool_call_log.jsonl` inside the
        // emitted package so the audit stays complete across long
        // sessions. Pre-emit sessions (no package yet) drop oldest
        // silently — the decisions.jsonl log is the authoritative
        // long-term audit, tool_call_log is the short-window trail.
        push_with_rotation(session, record);

        if is_mut && !res.is_error {
            session.last_activity = Utc::now();
        }

        results.push((id, res));
    }
    results
}

/// Dispatch a single tool call against the live session.
///
/// Wraps the per-tool handler with state-machine
/// consultation:
/// 1. Read [`ToolSpec::state_trigger`]; if `Some`, fire
///    [`Session::try_transition`] BEFORE the handler runs so the
///    handler executes against the new state. A failed transition
///    surfaces `ToolError::PreconditionFailure` and the handler is
///    skipped.
/// 2. Invoke the handler.
/// 3. Read [`ToolSpec::post_handler`]; if `Some`, fire
///    `on_ok(&mut session)` on success or `on_err(&mut session, &err)`
///    on failure.
///
/// Handlers themselves no longer call `try_transition` — the spec
/// table is the single auditability surface for the closed-vocab ×
/// state-machine contract pair. The
/// `state_machine_centralization::all_handlers_state_clean`
/// regression test asserts zero residual `try_transition(` calls in
/// `tools/<handler>.rs` files (excluding `mod.rs` shared helpers).
#[tracing::instrument(
    skip(session, ctx),
    fields(tool = tool.name(), alone_in_turn = tool.is_alone_in_turn())
)]
pub async fn dispatch_one(tool: &Tool, session: &mut Session, ctx: &ToolContext) -> ToolResult {
    let spec = tool.spec();
    // ── Idempotent emit_package short-circuit ─────────────────────────
    // When `state == Emitted` and a durable package is already on disk,
    // a second `emit_package` tool call returns the cached
    // `emitted_package_path` as a successful no-op instead of writing
    // a new timestamped package directory. The UI auto-sends a
    // synthetic `(confirmed — please continue)` turn after the SME
    // clicks Confirm; that turn drives the LLM's tool loop while the
    // session is already in Emitted (the auto-emit fired from
    // `/confirm`), and a model that picks `emit_package` again would
    // otherwise produce a byte-identical duplicate package ~60-70s
    // later, double on-disk footprint, and a phantom second
    // `kind: emit_package` row in `runtime/decisions.jsonl`.
    //
    // The guard sits at the dispatch boundary so every caller
    // (`dispatch_batch`, `dispatch_one`, future entry points) is
    // protected — not at the UI, which can only block the specific
    // synthetic-turn path. The state-machine itself is unchanged
    // (`(Emitted, EmitPackageStart) => Emitted` is a no-op
    // absorption); we just refuse to re-run the handler.
    //
    // amend_stage_method / branch_session both transition out of
    // Emitted (and clear `emitted_package_path` on branch), so the
    // legitimate re-emit cycle still runs the full handler.
    if matches!(tool, Tool::HighImpact(HighImpactTool::EmitPackage { .. }))
        && matches!(session.state, crate::session::SessionState::Emitted)
    {
        if let Some(existing) = session.emitted_package_path.as_ref() {
            tracing::info!(
                session_id = %session.id,
                output_dir = %existing.display(),
                "emit_package_noop_already_emitted: returning cached package path \
                 instead of writing a duplicate"
            );
            return ToolResult::ok(serde_json::json!({
                "ok": true,
                "output_dir": existing.to_string_lossy(),
                "noop": true,
                "reason": "session already emitted; returning cached package path",
            }));
        }
    }
    // ── Pre-handler state-machine consultation ────────────────────────
    if let Some(trigger_spec) = spec.state_trigger.as_ref() {
        match trigger_spec {
            StateTriggerSpec::Static(t) => {
                if let Err(e) = session.try_transition(t.clone()) {
                    return ToolResult::err(ToolError::PreconditionFailure {
                        reason: format!("state-machine rejected the transition: {}", e),
                        hint: format!(
                            "tool `{}` requires a session state from which the trigger is legal",
                            spec.name
                        ),
                    });
                }
            }
            StateTriggerSpec::Dynamic(f) => {
                if let Some(trigger) = f(session) {
                    if let Err(e) = session.try_transition(trigger) {
                        return ToolResult::err(ToolError::PreconditionFailure {
                            reason: format!("state-machine rejected the transition: {}", e),
                            hint: format!(
                                "tool `{}` requires a session state from which the trigger is legal",
                                spec.name
                            ),
                        });
                    }
                }
            }
            StateTriggerSpec::TolerantStatic(t) => {
                // Fire-and-forget. Matches the legacy
                // `let _ = session.try_transition(...)` discipline used
                // by AppendProse / ProposeSummaryConfirmation pre-rebuild
                // bookkeeping, but illegal cases are logged.
                if let Err(err) = session.try_transition(t.clone()) {
                    warn_illegal_transition(session, t, &err);
                }
            }
        }
    }
    // ── Invoke the per-variant handler ────────────────────────────────
    let result = dispatch_inner(tool, session, ctx).await;
    // ── Record classification confidence for is_ambiguous ────────────
    // After `AppendIntakeProse` (the tool that actually sets
    // `session.classification`), if the call succeeded and the session
    // now carries a `ClassificationResult`, record its confidence in the
    // MetricsStore. This is the canonical signal for Tier 16.2's
    // ambiguity flag (`confidence < 0.6 ⇒ is_ambiguous = true`).
    // Best-effort: a metrics IO failure never fails the tool dispatch.
    if matches!(
        tool,
        Tool::Batchable(BatchableTool::AppendIntakeProse { .. })
    ) && !result.is_error
    {
        if let (Some(store), Some(session_id), Some(clf)) = (
            ctx.metrics_store.as_ref(),
            ctx.session_id,
            session.classification.as_ref(),
        ) {
            let confidence = clf.confidence as f64;
            let store_clone = store.clone();
            tokio::spawn(async move {
                store_clone
                    .record_classification_confidence(session_id, confidence)
                    .await;
            });
        }
    }
    // ── Tier 17.x violation log (non-blocking, fire-and-forget) ──────
    // Check the result for known dispatcher-level rejection patterns and
    // append to `runtime/vocabulary-violations.jsonl` when matched.
    // This block MUST NOT alter `result` or `session` — it is purely
    // observational. All I/O errors are swallowed inside `emit()`.
    if result.is_error {
        let runtime_root = violation_runtime_root(session);
        // PrematureEmitAttempt — emit_package rejected because the SME
        // has not confirmed the current plan shape.
        //
        // The precondition is `is_confirmed()` (token +
        // pending_emission_id + summary_hash drift check). The error
        // reason starts with "SME has not confirmed" instead of
        // "user_confirmed is false"; we match either prefix so a
        // legacy session whose error path still produces the old text
        // (e.g. cached responses) still maps to the same violation
        // category.
        if matches!(tool, Tool::HighImpact(HighImpactTool::EmitPackage { .. })) {
            if let Ok(ToolError::PreconditionFailure { ref reason, .. }) =
                serde_json::from_value::<ToolError>(result.content.clone())
            {
                if reason.contains("user_confirmed")
                    || reason.contains("SME has not confirmed")
                    || reason.contains("re-confirmation required")
                {
                    violation_log::emit(
                        &violation_log::VocabularyViolation {
                            session_id: session.id.to_string(),
                            timestamp_ms: violation_log::now_ms(),
                            kind: violation_log::ViolationKind::PrematureEmitAttempt,
                            model: ctx.model.clone(),
                            tool_name: Some("emit_package".to_string()),
                            detail: "emit_package called before SME confirmation \
                                     (or after confirmation but plan summary drifted)"
                                .to_string(),
                        },
                        &runtime_root,
                    );
                }
            }
        }
        // InventedStageId — any tool that returns a ValidationFailure
        // whose reason matches the "not in this plan" / "unknown stage"
        // patterns produced by `ToolError::unknown_stage()` and the
        // taxonomy-stage check in `intake::set_intake_field`.
        // Covers set_intake_field, set_intake_method, amend_stage_method,
        // and validate_discover_stage rejections.
        if let Ok(err) = serde_json::from_value::<ToolError>(result.content.clone()) {
            let is_stage_error = match &err {
                ToolError::ValidationFailure { reason, .. } => {
                    reason.contains("not in this plan")
                        || reason.contains("unknown stage")
                        || reason.contains("stage not found")
                }
                ToolError::PreconditionFailure { reason, .. } => {
                    reason.contains("unknown stage") || reason.contains("stage not found")
                }
                _ => false,
            };
            if is_stage_error {
                violation_log::emit(
                    &violation_log::VocabularyViolation {
                        session_id: session.id.to_string(),
                        timestamp_ms: violation_log::now_ms(),
                        kind: violation_log::ViolationKind::InventedStageId,
                        model: ctx.model.clone(),
                        tool_name: Some(tool.name().to_string()),
                        detail: format!(
                            "tool `{}` referenced a stage id not found in the current DAG",
                            tool.name()
                        ),
                    },
                    &runtime_root,
                );
            }
        }
    }
    // Universal tool-error log. Surfaces every ToolError so corpus
    // post-mortem of `post_confirm_blocked` failures can map back to
    // the failing tool + variant + reason. Cost when unsubscribed is
    // a single discriminant check; the JSON re-parse only runs in
    // the error branch.
    if result.is_error {
        if let Ok(err) = serde_json::from_value::<ToolError>(result.content.clone()) {
            tracing::warn!(
                session_id = %session.id,
                tool = %tool.name(),
                error_variant = ?std::mem::discriminant(&err),
                error = ?err,
                "tool_call_error",
            );
        }
    }
    // ── Post-handler hooks ────────────────────────────────────────────
    if let Some(post) = spec.post_handler.as_ref() {
        if !result.is_error {
            if let Some(on_ok) = post.on_ok {
                on_ok(session);
            }
        } else if let Some(on_err) = post.on_err {
            // Reconstruct a ToolError view from the result envelope.
            // The result.content carries the serialized ToolError;
            // try to re-deserialize it so the on_err callback can
            // inspect the variant-specific reason. If parsing fails
            // (very rare — content is always set by ToolResult::err),
            // fall back to a generic InternalError.
            let err = serde_json::from_value::<ToolError>(result.content.clone()).unwrap_or(
                ToolError::InternalError {
                    reason: "post-handler couldn't parse tool error".into(),
                },
            );
            on_err(session, &err);
        }
    }
    result
}

async fn dispatch_inner(tool: &Tool, session: &mut Session, ctx: &ToolContext) -> ToolResult {
    // D11 — proposal-signoff lifecycle freshness. The
    // `propose_summary_confirmation` precondition reads
    // `session.proposals` from the tool-loop's local snapshot, which
    // was cloned out of the store at turn-start. If the SME clicks
    // Approve on a proposal mid-turn (or between the local snapshot
    // and dispatch), `/api/.../proposal/:id/signoff` writes
    // `Promoted` to the persisted store while the local snapshot
    // still reads `AwaitingSignoff`, producing the live-session bug
    // where the LLM tells the SME "please click Approve" even though
    // the SME just clicked Approve. Mirrors the existing precedent
    // in `emit_package` (tools/emit.rs:108) which re-reads `is_confirmed()`
    // + `proposals` from the store at gate time.
    //
    // Narrowed to `propose_summary_confirmation` to avoid breaking
    // the LLM tool-result caching invariant: read-only tools like
    // `get_session_state` continue to return the (potentially) stale
    // local snapshot, so Sonnet's prompt cache stays valid across
    // iterations.
    if let Tool::Batchable(BatchableTool::ProposeSummaryConfirmation { .. }) = tool {
        refresh_proposal_lifecycles_from_store(session, ctx).await;
    }
    // Top-level dispatch unwraps the bucket and forwards to the
    // bucket-specific handler. Each bucket's dispatch carries
    // `#[deny(non_exhaustive_omitted_patterns)]` on its match so a
    // new variant in either bucket fails at lint level — pairs with
    // the runtime `tool_schemas_consistency::names_match_enum` check.
    match tool {
        Tool::Batchable(b) => dispatch_batchable(b, session, ctx),
        Tool::HighImpact(h) => dispatch_high_impact(h, session, ctx).await,
    }
}

/// Refresh the lifecycle of any proposal whose id exists in the
/// persisted store, in place on the local `session.proposals` map.
/// Locally-minted proposals (created by an earlier
/// `propose_hypothesized_node` in the same tool loop, not yet flushed
/// to the store) are preserved untouched — they get persisted at the
/// post-loop merge in `send_turn`.
///
/// No-op when the store handle is not wired (CLI / unit-test paths)
/// or when the persisted session is missing.
///
/// Safe to call from the dispatch path: the store's `get` clones the
/// session out of its per-session mutex and releases the lock before
/// returning. The dispatch path holds no store lock, so re-entrancy
/// against `tokio::sync::Mutex` is not possible.
async fn refresh_proposal_lifecycles_from_store(session: &mut Session, ctx: &ToolContext) {
    let Some(store) = ctx.store.as_ref() else {
        return;
    };
    let Some(fresh) = store.get(session.id).await else {
        return;
    };
    for (id, local_prop) in session.proposals.iter_mut() {
        if let Some(fresh_prop) = fresh.proposals.get(id) {
            if !std::mem::discriminant(&local_prop.lifecycle)
                .eq(&std::mem::discriminant(&fresh_prop.lifecycle))
            {
                tracing::debug!(
                    session_id = %session.id,
                    proposal_id = %id,
                    from = %local_prop.lifecycle.kind_str(),
                    to = %fresh_prop.lifecycle.kind_str(),
                    "proposal_lifecycle_refreshed_from_store",
                );
            }
            local_prop.lifecycle = fresh_prop.lifecycle.clone();
            local_prop.last_transition_at = fresh_prop.last_transition_at;
            local_prop.gate_outcomes = fresh_prop.gate_outcomes.clone();
        }
    }
}

fn dispatch_batchable(
    tool: &BatchableTool,
    session: &mut Session,
    ctx: &ToolContext,
) -> ToolResult {
    #[allow(unknown_lints)]
    #[deny(non_exhaustive_omitted_patterns)]
    match tool {
        // ── Read-only ──
        BatchableTool::ClassifyIntake { prose } => {
            classification::classify_intake(prose, &ctx.config_dir)
        }
        BatchableTool::GetTaxonomyInfo { modality_id } => {
            taxonomy::get_taxonomy_info(modality_id, &ctx.config_dir)
        }
        BatchableTool::GetSessionState => result::get_session_state(session),
        BatchableTool::GetClassificationEvidence => {
            classification::get_classification_evidence(session)
        }
        BatchableTool::GetTaskResult { task_id } => result::get_task_result(session, task_id),
        BatchableTool::GetLiteratureContext {
            entity,
            entity_kind,
        } => literature_context::get_literature_context(session, entity, entity_kind.clone()),
        BatchableTool::ListAtoms { args } => atoms::list_atoms(args, &ctx.config_dir),
        // ── Intake mutation ──
        BatchableTool::SetIntakeField {
            stage,
            field,
            value,
        } => intake::set_intake_field(session, stage, field, value, &ctx.config_dir),
        BatchableTool::SetIntakeMethod {
            stage,
            method_prose,
        } => intake::set_intake_method(session, stage, method_prose, &ctx.config_dir),
        BatchableTool::AppendIntakeProse { prose } => {
            intake::append_intake_prose(session, prose, &ctx.config_dir)
        }
        BatchableTool::SetIntakeExcludedAtoms { atom_ids } => {
            intake::set_intake_excluded_atoms(session, atom_ids, &ctx.config_dir)
        }
        BatchableTool::SetIntakeModality { modality_id } => {
            intake::set_intake_modality(session, modality_id, &ctx.config_dir)
        }
        // ── Conversational control ──
        BatchableTool::ProposeSummaryConfirmation { summary_markdown } => {
            conversational::propose_summary_confirmation(session, summary_markdown)
        }
        BatchableTool::ProposeQuickReplies { question, options } => {
            conversational::propose_quick_replies(session, question, options, &ctx.config_dir)
        }
    }
}

async fn dispatch_high_impact(
    tool: &HighImpactTool,
    session: &mut Session,
    ctx: &ToolContext,
) -> ToolResult {
    #[allow(unknown_lints)]
    #[deny(non_exhaustive_omitted_patterns)]
    match tool {
        // ── Post-emission mutation ──
        HighImpactTool::AmendStageMethod {
            stage,
            method_prose,
            rationale,
        } => amend::amend_stage_method(
            session,
            stage,
            method_prose,
            rationale.as_deref(),
            &ctx.config_dir,
        ),
        HighImpactTool::SelectSensitivityWinner {
            stage,
            winner,
            rationale,
        } => sensitivity::select_sensitivity_winner(
            session,
            stage,
            winner,
            rationale.as_deref(),
            &ctx.config_dir,
        ),
        HighImpactTool::RerunTask { task_id, reason } => {
            let res = execution::rerun_task(session, task_id, reason.as_deref(), &ctx.config_dir);
            // notify any server-side artifact cache so the
            // stale listing is dropped ahead of the agent's re-run.
            if !res.is_error {
                if let Some(sink) = &ctx.event_sink {
                    sink.task_reset(session.id, task_id);
                }
            }
            res
        }
        HighImpactTool::BranchSession { rationale } => {
            branch::branch_session(session, rationale.as_deref())
        }
        // ── Emit + execution ──
        HighImpactTool::EmitPackage { output_dir } => {
            emit::emit_package(
                session,
                output_dir.as_deref(),
                &ctx.config_dir,
                ctx.metrics_store.as_ref(),
                ctx.store.as_ref(),
            )
            .await
        }
        HighImpactTool::StartExecution { max_iterations } => {
            execution::start_execution_tool(session, *max_iterations, ctx)
        }
        HighImpactTool::ProposeHypothesizedNode {
            proposed_id,
            intent,
            parent_terms,
            llm_rationale,
            assumptions,
            failure_modes,
            validation_tests,
            upstream_atom_ids,
        } => hypothesized_node::propose_hypothesized_node(
            session,
            proposed_id,
            intent,
            parent_terms,
            llm_rationale,
            assumptions,
            failure_modes,
            validation_tests,
            upstream_atom_ids,
            ctx,
        ),
        HighImpactTool::ProposeHypothesizedRenderer {
            target_semantic_type,
            proposed_parent_terms,
            proposed_figure_ids,
            sme_intent,
            primitive_basis,
        } => {
            // Load the PlotAffordanceRegistry from the config dir on
            // every call (sub-millisecond for the current small catalog;
            // the registry is not cached on ToolContext to keep blast
            // radius minimal). Falls back to an empty registry on load
            // error so the tool can still reject with a clear error
            // rather than panicking.
            use ecaa_workflow_core::plot_affordance::YamlPlotAffordanceRegistry;
            let plot_dir = ctx.config_dir.join("plot-affordances");
            let registry: Box<dyn ecaa_workflow_core::plot_affordance::PlotAffordanceRegistry> =
                match YamlPlotAffordanceRegistry::from_dir(&plot_dir) {
                    Ok(r) => Box::new(r),
                    Err(e) => {
                        tracing::warn!(
                            config_dir = %ctx.config_dir.display(),
                            error = %e,
                            "propose_hypothesized_renderer: PlotAffordanceRegistry load failed; \
                             proceeding with empty registry (all parent-term checks will reject)"
                        );
                        Box::new(YamlPlotAffordanceRegistry::empty())
                    }
                };
            hypothesized_renderer::propose_hypothesized_renderer(
                session,
                registry.as_ref(),
                target_semantic_type,
                proposed_parent_terms,
                proposed_figure_ids,
                sme_intent,
                primitive_basis.as_deref(),
            )
        }
    }
}

// ── Shared helpers (consumed by submodules via `super::`) ────────────────────

/// in-memory audit cap. Tool calls beyond this rotate to
/// `runtime/tool_call_log.jsonl` for emitted sessions; pre-emit
/// sessions drop-oldest silently (decisions.jsonl is the long-term
/// audit in that case).
const TOOL_CALL_LOG_CAP: usize = 1000;

/// append a tool-call audit record with cap-and-rotate.
/// - Always pushes the new record.
/// - If the in-memory log exceeds TOOL_CALL_LOG_CAP, pops the oldest.
/// - If the session has `emitted_package_path`, appends the evicted
///   record to `<root>/runtime/tool_call_log.jsonl` so the audit
///   stays complete across long sessions.
pub(super) fn push_with_rotation(session: &mut Session, record: ToolCallRecord) {
    session.tool_call_log.push(record);
    while session.tool_call_log.len() > TOOL_CALL_LOG_CAP {
        let evicted = session.tool_call_log.remove(0);
        if let Some(root) = &session.emitted_package_path {
            let runtime_dir = root.join("runtime");
            let _ = std::fs::create_dir_all(&runtime_dir);
            let path = runtime_dir.join("tool_call_log.jsonl");
            if let Ok(line) = serde_json::to_string(&evicted) {
                use std::io::Write;
                if let Ok(mut f) = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&path)
                {
                    let _ = writeln!(f, "{}", line);
                }
            }
        }
    }
}

/// Resolve the `runtime/` directory to use for violation-log writes.
///
/// Priority:
/// 1. `<session.emitted_package_path>/runtime/` — post-emit sessions get
///    the violation appended alongside the other runtime artefacts.
/// 2. `$ECAA_CHAT_SESSIONS_DIR/<session_id>/runtime/` — pre-emit sessions
///    write to the sessions dir so violations are always captured even
///    before a package exists on disk.
/// 3. `/tmp/scripps-violations/<session_id>/runtime/` — emergency fallback
///    when neither env-var nor home dir is resolvable.
pub(super) fn violation_runtime_root(session: &Session) -> std::path::PathBuf {
    if let Some(pkg) = &session.emitted_package_path {
        return pkg.join("runtime");
    }
    let sessions_dir = std::env::var("ECAA_CHAT_SESSIONS_DIR").unwrap_or_else(|_| {
        std::env::var("HOME")
            .map(|h| format!("{}/.ecaa-workflow/sessions", h))
            .unwrap_or_else(|_| "/tmp/scripps-violations".to_string())
    });
    std::path::PathBuf::from(sessions_dir)
        .join(session.id.to_string())
        .join("runtime")
}

/// Build the DAG via the v4 composer, with the cross-omics single-modality
/// retry. On the first composer success returns the DAG. When the composer
/// returns `None` and the intake requires cross-omics, clears
/// `additional_modalities` and retries once (rescuing 3-way scenarios the
/// set-equality cross-omics matcher under-counted); a second `None` — or a
/// `None` on a non-cross-omics intake — invalidates the cache and returns the
/// matching precondition failure.
fn compose_dag_with_fallback(
    session: &mut Session,
    config_dir: &std::path::Path,
    requires_cross_omics: bool,
) -> Result<ecaa_workflow_core::dag::DAG, ToolError> {
    if let Some(dag) = try_build_via_composer(session, config_dir) {
        tracing::debug!(
            session_id = %session.id,
            composer_version = session.composer_version,
            "rebuild_dag: composer fast-path — built DAG via build_dag_from_composition"
        );
        return Ok(dag);
    }
    if requires_cross_omics {
        tracing::warn!(
            session_id = %session.id,
            "rebuild_dag: cross-omics composer returned None — retrying with single-modality fallback (additional_modalities cleared)"
        );
        if let Some(classification) = session.classification.as_mut() {
            classification.additional_modalities.clear();
        }
        if let Some(dag) = try_build_via_composer(session, config_dir) {
            tracing::warn!(
                session_id = %session.id,
                "rebuild_dag: cross-omics retry succeeded as single-modality (degraded path)"
            );
            return Ok(dag);
        }
        #[allow(deprecated)] // deliberate cache-reset for non-workflow_dag state change
        session.invalidate_dag();
        return Err(ToolError::PreconditionFailure {
            reason: "explicit cross-omics intake could not be composed into a multi-branch DAG"
                .into(),
            hint: "Keep both requested omics layers in scope: author or fix a matching \
                   cross-omics archetype, or ask the SME to choose a single modality before \
                   emitting."
                .into(),
        });
    }
    #[allow(deprecated)] // deliberate cache-reset for non-workflow_dag state change
    session.invalidate_dag();
    Err(ToolError::PreconditionFailure {
        reason: "composer could not produce a DAG for this intake".into(),
        hint: "Verify the modality keyword and goal phrase resolve to an archetype \
               in config/archetypes/. The legacy taxonomy fallback was removed in \
               Phase 6.1 B4 of the closure plan."
            .into(),
    })
}

/// Apply the SME's `set_intake_excluded_atoms` selection as a post-composition
/// prune on the authoritative `session.workflow_dag`. Drops every excluded node
/// (matching the node id or its `discover_`/`validate_`-stripped base); for a
/// surviving node whose every incoming edge came from a dropped node, REWIRES it
/// to `data_acquisition` (the SME's data becomes the new input source) rather
/// than cascade-dropping — unless `data_acquisition` itself was dropped. Then
/// re-derives `session.dag` from the pruned workflow_dag. No-op when nothing is
/// excluded or no workflow_dag is present.
fn prune_excluded_atoms(session: &mut Session) {
    if session.excluded_atoms.is_empty() {
        return;
    }
    let excluded: std::collections::BTreeSet<String> =
        session.excluded_atoms.iter().cloned().collect();
    let Some(wf) = session.workflow_dag.as_mut() else {
        return;
    };
    let mut dropped: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for node in wf.nodes.iter() {
        let base = node
            .id
            .strip_prefix("discover_")
            .or_else(|| node.id.strip_prefix("validate_"))
            .unwrap_or(node.id.as_str());
        if excluded.contains(node.id.as_str()) || excluded.contains(base) {
            dropped.insert(node.id.clone());
        }
    }
    // data_acquisition is the synthetic upstream source for any orphaned
    // downstream atom when the SME's data is post-pipeline. If it was itself
    // dropped (unusual), fall back to cascade-drop.
    let data_acq_id = "data_acquisition";
    let data_acq_present =
        wf.nodes.iter().any(|n| n.id == data_acq_id) && !dropped.contains(data_acq_id);
    // Rewire-or-drop pass (single sweep; the rewire makes the graph stable so
    // no fixpoint is needed).
    let mut rewires: Vec<(String, String)> = Vec::new();
    for node in wf.nodes.iter() {
        if dropped.contains(&node.id) {
            continue;
        }
        let incoming: Vec<&str> = wf
            .edges
            .iter()
            .filter(|e| e.to_node == node.id)
            .map(|e| e.from_node.as_str())
            .collect();
        if incoming.is_empty() {
            continue;
        }
        let all_dropped = incoming.iter().all(|src| dropped.contains(*src));
        if !all_dropped {
            continue;
        }
        if data_acq_present && node.id != data_acq_id {
            rewires.push((data_acq_id.to_string(), node.id.clone()));
        } else {
            dropped.insert(node.id.clone());
        }
    }
    // Drop edges referencing dropped nodes, then drop the nodes.
    wf.edges
        .retain(|e| !dropped.contains(&e.from_node) && !dropped.contains(&e.to_node));
    wf.nodes.retain(|n| !dropped.contains(&n.id));
    // Add rewire edges (after dropping the now-dead originals). Minimal
    // EdgeContract with sentinel ports + a rationale so downstream lowering
    // picks up the edge.
    for (from_node, to_node) in rewires {
        let already = wf
            .edges
            .iter()
            .any(|e| e.from_node == from_node && e.to_node == to_node);
        if already {
            continue;
        }
        let proof = ecaa_workflow_core::workflow_contracts::edge::CompatibilityProof {
            rationale: Some(
                "rewired to data_acquisition because upstream atom(s) were excluded by SME"
                    .to_string(),
            ),
            ..Default::default()
        };
        wf.edges
            .push(ecaa_workflow_core::workflow_contracts::edge::EdgeContract {
                from_node,
                from_port: "_excluded_rewire".into(),
                to_node,
                to_port: "_excluded_rewire".into(),
                proof,
                chain_of_custody: None,
            });
    }
    tracing::debug!(
        session_id = %session.id,
        dropped_count = dropped.len(),
        excluded_count = excluded.len(),
        "rebuild_dag: pruned excluded atoms"
    );
    let id = workflow_id(&session.id);
    if let Ok(rebuilt) = ecaa_workflow_core::builder::build_dag_from_workflow_dag(wf, &id) {
        session.dag = Some(rebuilt);
    }
}

pub(crate) fn rebuild_dag(
    session: &mut Session,
    config_dir: &std::path::Path,
) -> Result<(), ToolError> {
    if session.taxonomy.is_none() {
        return Err(ToolError::PreconditionFailure {
            reason: "no taxonomy loaded — append_intake_prose first to classify".into(),
            hint: "Call append_intake_prose with the SME's description.".into(),
        });
    }
    let _methods = session.intake_methods.to_core();
    let requires_cross_omics = session
        .classification
        .as_ref()
        .map(|c| !c.additional_modalities.is_empty())
        .unwrap_or(false);

    // The legacy `build_dag_from_taxonomy` fallback was retired with
    // `config/stage-taxonomies/`. v4 is the only path:
    // - Cross-omics gaps are closed by archetype catalog additions
    // (time_series_forecast + generic_omics).
    // - Discovery-flow gaps are closed by `discover_*` companion
    // synthesis in `composer_v4::discover_companion_synthesis`.
    //
    // Sessions persisted with `composer_version == 1` are now routed
    // through v4 too: the persisted `taxonomy` metadata is preserved
    // for round-trip, but the DAG comes from the composer.
    let dag = compose_dag_with_fallback(session, config_dir, requires_cross_omics)?;

    // The state machine maps
    // (Intake, DagBuiltWithUnresolvedDiscovery) → IntakeFollowup
    // Every mutation tool funnels through rebuild_dag, so this is the
    // right place to surface "the DAG has unresolved discovery work".
    // Typed role check via `derive_role_from_id`. The
    // DAG's `Task` doesn't carry an `AtomRole` field today, so we
    // derive from the task id. Once Task gains a typed role field,
    // switch this to `t.role.is_discovery()`.
    let has_unresolved = dag.tasks.iter().any(|(id, t)| {
        ecaa_workflow_core::taxonomy::derive_role_from_id(id.as_str()).is_discovery()
            && matches!(t.state, TaskState::Pending | TaskState::Ready)
    });

    // Phase D refactor: populate the cache eagerly so the immediate
    // downstream reads in this turn (and the next API request) don't
    // re-lower. The cache will be invalidated by the next rebuild_dag,
    // by `invalidate_and_rebuild`, or by the send_turn merge.
    //
    // `task_states` overlay is applied at read time via `current_dag()`,
    // so we don't need to walk and apply states here.
    session.dag = Some(dag);

    // Sub-archetype small-task pruning. The SME may have flagged
    // upstream pipeline atoms as excluded via `set_intake_excluded_atoms`.
    // The v4 composer doesn't yet thread `IntakeContext.excluded_atoms`
    // through to its forward/backward port-based search, so we apply
    // the exclusion as a post-composition prune on the AUTHORITATIVE
    // structure (`session.workflow_dag.nodes` + `.edges`).
    //
    // Algorithm:
    //   1. Drop every node whose id is in the exclusion set (or whose
    //      `discover_<id>` / `validate_<id>` prefix-strip yields one).
    //   2. For each surviving node whose every incoming edge came from
    //      a dropped node, REWIRE it to `data_acquisition` rather than
    //      cascade-dropping it. The user's data acts as the new input
    //      source — e.g. Cell Ranger output flows into qc_preprocessing
    //      directly when the user excludes quantification. Only cascade-
    //      drop a surviving node when `data_acquisition` itself was
    //      dropped (rare; preserves the orphan-cleanup property).
    //   3. Re-derive `session.dag` from the pruned workflow_dag so the
    //      next read sees the trimmed surface immediately.
    prune_excluded_atoms(session);

    // Re-inject any SME-promoted nodes that were
    // spliced into `session.workflow_dag` before this rebuild ran.
    // See `reinject_promoted_nodes_into_workflow_dag` for the full
    // explanation and the D4 chained-proposal regression test.
    reinject_promoted_nodes_into_workflow_dag(session);

    if has_unresolved {
        // Best-effort: from terminal states (Emitted, Blocked, ReadyToEmit
        // etc.) the trigger is illegal and try_transition returns Err. We
        // intentionally swallow that — the trigger only matters from
        // Intake/IntakeFollowup, which are exactly the states a mutation
        // tool can be called from in the normal flow.
        let trigger = crate::session::StateTrigger::DagBuiltWithUnresolvedDiscovery;
        if let Err(err) = session.try_transition(trigger.clone()) {
            warn_illegal_transition(session, &trigger, &err);
        }
    }

    Ok(())
}

/// Re-inject every `Promoted` proposal back into `session.workflow_dag`
/// after a composer rebuild. `try_build_via_composer` replaces
/// `session.workflow_dag` from the composer's output (it builds from
/// intake classification, not from the existing dag nodes); without
/// this pass a `rebuild_dag` call after proposal signoff would silently
/// evict every SME-approved node.
///
/// **Two-pass design.** Pass 1 pushes every Promoted node into
/// `dag.nodes`. Pass 2 wires upstream/downstream/validator edges, by
/// which point every promoted sibling is already present — so a
/// promoted-to-promoted edge (B's `upstream_atom_ids` naming a sibling
/// C, both freshly-promoted in the same signoff burst) resolves
/// regardless of HashMap iteration order over `session.proposals`. A
/// single-pass body iterating each proposal and immediately walking
/// its `upstream_atom_ids` would visit leaves before parents in some
/// orderings and silently drop the edge with a "upstream not in
/// current DAG" warning, lowering `depends_on=[]` despite the proposal
/// correctly naming the upstream.
/// Collect every `Promoted` proposal as `(task_node_id, (proposal, authority))`
/// for re-injection, stamping a `rebuild_reinject` `PromotionAuthority` whose id
/// is the most recent SME-signoff gate detail (falling back to `"sme"`).
#[allow(clippy::type_complexity)]
fn collect_promoted_for_reinject(
    session: &Session,
) -> Vec<(
    String,
    (
        ecaa_workflow_core::hypothesized_proposal::HypothesizedProposal,
        ecaa_workflow_core::workflow_contracts::lifecycle::PromotionAuthority,
    ),
)> {
    use ecaa_workflow_core::hypothesized_proposal::{GateName, ProposalLifecycle};
    session
        .proposals
        .values()
        .filter_map(|p| {
            let ProposalLifecycle::Promoted { task_node_id } = &p.lifecycle else {
                return None;
            };
            let authority = ecaa_workflow_core::workflow_contracts::lifecycle::PromotionAuthority {
                kind: "rebuild_reinject".into(),
                id: p
                    .gate_outcomes
                    .iter()
                    .rev()
                    .find_map(|g| {
                        if g.gate == GateName::SmeSignoff {
                            g.details.first().cloned()
                        } else {
                            None
                        }
                    })
                    .unwrap_or_else(|| "sme".to_string()),
                at: p.last_transition_at.to_string(),
            };
            Some((task_node_id.clone(), (p.clone(), authority)))
        })
        .collect()
}

/// Wire the upstream edges for one promoted node — one edge from each declared
/// `upstream_atom_ids` that exists in `dag` (missing upstreams are warned and
/// skipped). Idempotent. Returns `true` if any edge was added.
fn wire_upstream_edges(
    dag: &mut ecaa_workflow_core::workflow_contracts::task_node::WorkflowDag,
    session_id: uuid::Uuid,
    task_node_id: &str,
    proposal: &ecaa_workflow_core::hypothesized_proposal::HypothesizedProposal,
) -> bool {
    use ecaa_workflow_core::workflow_contracts::edge::{CompatibilityProof, EdgeContract};
    let mut dirty = false;
    for upstream_id in &proposal.upstream_atom_ids {
        let upstream_exists = dag.nodes.iter().any(|n| n.id == *upstream_id);
        if !upstream_exists {
            tracing::warn!(
                session_id = %session_id,
                proposal_id = %proposal.id,
                task_node_id = %task_node_id,
                upstream_atom_id = %upstream_id,
                "rebuild_dag: promoted proposal references upstream atom not in current DAG; skipping edge"
            );
            continue;
        }
        let already_edged = dag
            .edges
            .iter()
            .any(|e| e.from_node == *upstream_id && e.to_node == *task_node_id);
        if already_edged {
            continue;
        }
        let proof = CompatibilityProof {
            rationale: Some(format!(
                "promoted hypothesized node `{}` declared upstream `{}` via propose_hypothesized_node",
                task_node_id, upstream_id
            )),
            ..Default::default()
        };
        dag.edges.push(EdgeContract {
            from_node: upstream_id.clone(),
            from_port: "_promoted_upstream".into(),
            to_node: task_node_id.to_string(),
            to_port: "_promoted_input".into(),
            proof,
            chain_of_custody: None,
        });
        dirty = true;
    }
    dirty
}

/// Wire one promoted node into `dag`: upstream edges (from each declared
/// `upstream_atom_ids` present in the DAG), a downstream edge to the report
/// sink (`reporting` → `final_reporting` → `generic_summary`, first present —
/// the `generic_summary` fallback covers the `generic_omics` novel-method path
/// where out-of-catalog modalities drive analysis entirely via
/// `propose_hypothesized_node`), and a `validate_<id>` wrapper node + edge.
/// All steps are idempotent. Returns `true` if any structural change was made.
fn wire_promoted_node(
    dag: &mut ecaa_workflow_core::workflow_contracts::task_node::WorkflowDag,
    session_id: uuid::Uuid,
    task_node_id: &str,
    proposal: &ecaa_workflow_core::hypothesized_proposal::HypothesizedProposal,
) -> bool {
    use ecaa_workflow_core::workflow_contracts::edge::{CompatibilityProof, EdgeContract};
    use ecaa_workflow_core::workflow_contracts::evidence::ValidatorRef;
    use ecaa_workflow_core::workflow_contracts::implementation::Implementation;
    use ecaa_workflow_core::workflow_contracts::lifecycle::LifecycleState;
    use ecaa_workflow_core::workflow_contracts::task_node::TaskNode;

    let mut dirty = wire_upstream_edges(dag, session_id, task_node_id, proposal);

    let downstream_id = if dag.nodes.iter().any(|n| n.id == "reporting") {
        Some("reporting".to_string())
    } else if dag.nodes.iter().any(|n| n.id == "final_reporting") {
        Some("final_reporting".to_string())
    } else if dag.nodes.iter().any(|n| n.id == "generic_summary") {
        Some("generic_summary".to_string())
    } else {
        None
    };
    if let Some(d) = downstream_id {
        let already = dag
            .edges
            .iter()
            .any(|e| e.from_node == *task_node_id && e.to_node == d);
        if !already {
            let proof = CompatibilityProof {
                rationale: Some(format!(
                    "default downstream wiring: promoted hypothesized atom `{}` feeds `{}`",
                    task_node_id, d
                )),
                ..Default::default()
            };
            dag.edges.push(EdgeContract {
                from_node: task_node_id.to_string(),
                from_port: "_promoted_output".into(),
                to_node: d,
                to_port: "_promoted_downstream".into(),
                proof,
                chain_of_custody: None,
            });
            dirty = true;
        }
    }

    let validate_id = format!("validate_{task_node_id}");
    if !dag.nodes.iter().any(|n| n.id == validate_id) {
        let mut validator_node = TaskNode::skeleton(
            validate_id.clone(),
            format!("Validator wrapper for promoted hypothesized atom `{task_node_id}`"),
        );
        validator_node.lifecycle_state = LifecycleState::Contracted;
        validator_node.implementation = Implementation::Unimplemented;
        validator_node.validators = proposal
            .validation_tests
            .iter()
            .map(|id| ValidatorRef {
                id: id.clone(),
                version: None,
                parameters: None,
            })
            .collect();
        dag.nodes.push(validator_node);
        dirty = true;
    }
    let validate_edge_wired = dag
        .edges
        .iter()
        .any(|e| e.from_node == *task_node_id && e.to_node == validate_id);
    if !validate_edge_wired {
        let proof = CompatibilityProof {
            rationale: Some("validator wrapper for promoted hypothesized atom".to_string()),
            ..Default::default()
        };
        dag.edges.push(EdgeContract {
            from_node: task_node_id.to_string(),
            from_port: "_promoted_output".into(),
            to_node: validate_id,
            to_port: "_validator_input".into(),
            proof,
            chain_of_custody: None,
        });
        dirty = true;
    }

    dirty
}

pub(crate) fn reinject_promoted_nodes_into_workflow_dag(session: &mut Session) {
    use ecaa_workflow_core::hypothesized_proposal::proposal_to_materialized_task_node;
    let to_inject = collect_promoted_for_reinject(session);

    if to_inject.is_empty() {
        return;
    }
    // Track whether ANY structural change touched the workflow_dag — node
    // added, edge added, validator wrapper synthesized. Used after the
    // injection loop to decide whether `session.dag` (the lowered cache)
    // must be re-derived from the updated workflow_dag. Without this
    // counter, the `dag` cache populated upstream by `rebuild_dag` keeps
    // the COMPOSER-ONLY shape and emit_package serializes a WORKFLOW.json
    // that omits the promoted node — the exact failure-mode the proposal
    // signoff handler set out to prevent.
    let mut wf_dirty: bool = false;
    let sid = session.id;
    let Some(dag) = session.workflow_dag.as_mut() else {
        return;
    };

    let mut reinjected: usize = 0;
    // Pass 1 — push every promoted node so promoted→promoted edges in
    // Pass 2 can resolve regardless of iteration order.
    for (task_node_id, (proposal, authority)) in &to_inject {
        if !dag.nodes.iter().any(|n| n.id == *task_node_id) {
            tracing::debug!(
                session_id = %session.id,
                task_node_id = %task_node_id,
                proposal_id = %proposal.id,
                "rebuild_dag: re-injecting promoted proposal node into rebuilt WorkflowDag"
            );
            dag.nodes.push(proposal_to_materialized_task_node(
                proposal,
                authority.clone(),
            ));
            reinjected += 1;
            wf_dirty = true;
        }
    }
    // Pass 2 — wire edges now that every promoted node is present.
    for (task_node_id, (proposal, _authority)) in &to_inject {
        if wire_promoted_node(dag, sid, task_node_id, proposal) {
            wf_dirty = true;
        }
    }
    tracing::debug!(
        session_id = %session.id,
        reinjected = reinjected,
        wf_dirty = wf_dirty,
        "rebuild_dag: post-overlay re-injection"
    );

    // The reinject loop only mutated `session.workflow_dag` (the
    // authoritative typed DAG). `rebuild_dag` populated `session.dag`
    // (the lowered cache) earlier in the same call from the COMPOSER's
    // output — which does NOT include promoted proposals because the
    // composer builds from intake classification, not from the
    // existing dag nodes. Without the re-derive below, `emit_steps`
    // would read the stale cache and ship a WORKFLOW.json missing the
    // promoted task (the cell-cell-communication signoff sequence —
    // `rebuild_dag_after_signoff` overwrites `session.dag`, reinject
    // splices into `workflow_dag` only, then `emit_package` serialises
    // the stale lowered cache and the promoted task is lost).
    //
    // Re-derive the lowered cache from the freshly-updated workflow_dag.
    // Soft-fail mirrors the exclusion-pruning branch above: a lowering
    // error is logged but we leave the existing `session.dag` in place
    // rather than wiping it (so emit can still try its own
    // `ensure_dag_cached` fallback path).
    rederive_dag_cache_if_dirty(session, wf_dirty);
}

/// Re-derive the lowered `session.dag` cache from the (re-injection-updated)
/// authoritative `session.workflow_dag`, but only when `wf_dirty`. Soft-fail:
/// a lowering error is logged and leaves the existing `session.dag` in place so
/// emit can still try its own `ensure_dag_cached` fallback rather than shipping
/// a WORKFLOW.json missing the promoted nodes.
fn rederive_dag_cache_if_dirty(session: &mut Session, wf_dirty: bool) {
    if !wf_dirty {
        return;
    }
    let Some(wf) = session.workflow_dag.as_ref() else {
        return;
    };
    let id = workflow_id(&session.id);
    match ecaa_workflow_core::builder::build_dag_from_workflow_dag(wf, &id) {
        Ok(rebuilt) => {
            session.dag = Some(rebuilt);
        }
        Err(e) => {
            tracing::warn!(
                session_id = %session.id,
                error = %e,
                "rebuild_dag: post-reinject dag re-derive failed; \
                 leaving session.dag at its pre-reinject value"
            );
        }
    }
}

pub(super) fn workflow_id(session_id: &uuid::Uuid) -> String {
    format!("workflow-{}", session_id.as_simple())
}

/// Composer fast-path for sessions that
/// pinned an `archetype_snapshot` and committed to
/// `composer_version >= 2`. Loads `AtomRegistry` + `ArchetypeRegistry`
/// from `config/{stage-atoms,archetypes}/`, calls `compose_with_version`
/// with the session's `(goal, project_class, composer_version)`, and
/// hands the resulting `CompositionResult` to `build_dag_from_composition`.
///
/// Returns `None` (with a `tracing::warn!`) on every soft-fail path:
/// missing config dir, registry load error, composer error
/// (`CompositionInfeasible`, `TieRequiresSmeDecision`, …), or DAG
/// build error. The legacy `build_dag_from_taxonomy` is the fallback;
/// `rebuild_dag` runs it when this returns None so production stays
/// resilient even when the composer path has gaps.
/// Resolve a session's active policy bundle id to a
/// loaded `PolicyContext`. Returns `None` for unknown bundle ids
/// (logged as warning).
fn resolve_policy_bundle(
    bundle_id: &str,
) -> Option<ecaa_workflow_core::policy_context::PolicyContext> {
    use ecaa_workflow_core::policy_context::PolicyContext;
    let ctx = PolicyContext::empty();
    match bundle_id {
        "clinical_trial" => Some(ctx.with_bundle(PolicyContext::clinical_trial_bundle())),
        "phi_strict" => Some(ctx.with_bundle(PolicyContext::phi_strict_bundle())),
        other => {
            tracing::warn!(
                bundle_id = %other,
                "resolve_policy_bundle: unknown bundle id; falling back to no-policy compose"
            );
            None
        }
    }
}

fn try_build_via_composer(
    session: &mut crate::session::Session,
    config_dir: &std::path::Path,
) -> Option<ecaa_workflow_core::dag::DAG> {
    use ecaa_workflow_core::archetype_registry::ArchetypeRegistry;
    use ecaa_workflow_core::atom_registry::AtomRegistry;
    use ecaa_workflow_core::builder::{build_dag_from_composition, build_dag_from_workflow_dag};
    use ecaa_workflow_core::composer::compose_with_version_and_modalities_full;
    use ecaa_workflow_core::goal_spec::GoalSpec;
    use ecaa_workflow_core::project_class::ProjectClass;

    let classification = session.classification.as_ref()?;
    // Thread the
    // primary modality plus any cross-omics companions detected by
    // the classifier (M1) into the composer. When
    // `additional_modalities` is empty the multi entry point
    // delegates to the single-modality compose, preserving existing
    // single-modality behavior. When non-empty (cross-omics), the
    // composer reaches `find_match_cross_omics` and dispatches the
    // matching cross-omics archetype.
    let mut modalities: Vec<&str> = std::iter::once(classification.modality.as_str())
        .chain(
            classification
                .additional_modalities
                .iter()
                .map(|m| m.modality.as_str()),
        )
        .collect();

    // Novel-method override. When the SME's prose names a method
    // outside the archetype catalog — Mendelian randomization, CyTOF,
    // Slide-seq, snmC-seq, CODEX spatial proteomics, Cryo-EM,
    // cox-regression survival, microbiome strain-SNP, scATAC variants
    // — the classifier still picks the closest-keyword modality (e.g.
    // gwas → gwas_coloc for MR), which composes `colocalization` /
    // other modality-specific atoms the SME didn't ask for. Route
    // these to `generic_omics` whose `raw_qc → generic_summary`
    // scaffold lets the SME drive the analysis via amendments +
    // propose_hypothesized_node. Detection is conservative — only
    // explicit method names trigger the override, not infrastructure
    // mentions. List is curated by the paper-recreation corpus's
    // `flex-*` scenarios; adding a method here is a one-line change.
    {
        const NOVEL_METHOD_TOKENS: &[&str] = &[
            "mendelian randomization",
            "mendelian-randomization",
            "mr-egger",
            "mr egger",
            "two-sample mr",
            "two sample mr",
            "instrument variable",
            "instrumental variable",
            "mass cytometry",
            "cytof",
            "imc imaging",
            "imaging mass cytometry",
            "slide-seq",
            "slide seq",
            "slideseq",
            "stereo-seq",
            "stereo seq",
            "snmc-seq",
            "snmc seq",
            "snmcseq",
            "single-cell methylation",
            "single cell methylation",
            "codex spatial",
            "codex multiplexed",
            "co-detection by indexing",
            "cox regression",
            "cox proportional hazards",
            "cox model",
            "strain-level snp",
            "strain snp",
            "strain-level",
            "strain-resolved",
            "metagenomic strain",
            "strainphlan",
            "instrain",
            "midas strain",
            "kaplan meier",
            "kaplan-meier",
            "log-rank",
            "log rank test",
            "proportional-hazards",
            "proportional hazards",
            "time-to-event",
            "scatac-seq",
            "scatac seq",
            "scatacseq",
            "snatac-seq",
            "snatac seq",
            "single-cell atac",
            "single cell atac",
            "single-nucleus atac",
            "single nucleus atac",
            "cryo-em",
            "cryo em",
            "cryoem",
            "single particle reconstruction",
            "single-particle reconstruction",
        ];
        // Normalize prose to fold hyphens / mixed case so SME phrasings
        // like "Cox proportional-hazards model", "STRAIN-level", and
        // "single-cell ATAC" all map to the canonical token list.
        let prose_norm =
            ecaa_workflow_core::classify::normalize_for_match(&classification.intake_text);
        let novel_hit = NOVEL_METHOD_TOKENS.iter().find(|t| {
            let needle = ecaa_workflow_core::classify::normalize_for_match(t);
            !needle.is_empty() && prose_norm.contains(&needle)
        });
        if let Some(token) = novel_hit {
            // Paired-protocol guard: multiome / SHARE-seq prompts
            // mention "single-nucleus atac" alongside "snRNA"; those
            // tokens overlap with the novel-method list but belong to
            // the cross-omics archetype path, NOT generic_omics. Skip
            // the override when the prose AFFIRMATIVELY names a
            // paired protocol. Negation guards
            // ("DIFFERENT from SHARE-seq", "no paired RNA") prevent
            // pure-scATAC prompts from being misclassified as
            // paired-protocol via the protocol token mention.
            let intake_lower = classification.intake_text.to_lowercase();
            let names_paired_protocol = (intake_lower.contains("multiome")
                || intake_lower.contains("share-seq")
                || intake_lower.contains("share seq")
                || intake_lower.contains("shareseq"))
                && !intake_lower.contains("different from share-seq")
                && !intake_lower.contains("different from multiome")
                && !intake_lower.contains("not share-seq")
                && !intake_lower.contains("not multiome")
                && !intake_lower.contains("no paired rna")
                && !intake_lower.contains("no paired");
            if classification.modality != "generic_omics" && !names_paired_protocol {
                tracing::warn!(
                    session_id = %session.id,
                    classified_modality = %classification.modality,
                    novel_token = %token,
                    "novel_method_detected_routing_to_generic_omics"
                );
                modalities = vec!["generic_omics"];
            }
        }
    }
    let project_class_for_composer = session
        .taxonomy
        .as_ref()
        .and_then(|t| t.project_class)
        .unwrap_or(session.project_class);
    let project_class_str = match project_class_for_composer {
        ProjectClass::Bioinformatics => "bioinformatics",
        ProjectClass::ClinicalTrial => "clinical_trial",
        ProjectClass::TimeSeriesForecast => "time_series_forecast",
    };

    // Config_dir is threaded from the ToolContext through every
    // handler that calls `rebuild_dag`, so the composer's archetype +
    // atom registry loads find the workspace config regardless of
    // whether the test harness sets CWD to the workspace root or a
    // per-crate dir. Reading ECAA_CONFIG_DIR with a "./config"
    // fallback here would break under `cargo test -p
    // ecaa-workflow-conversation` (the per-crate CWD has no
    // `config/` sibling) and mask real composer dispatch errors.
    let atom_dir = config_dir.join("stage-atoms");
    let archetype_dir = config_dir.join("archetypes");
    let atoms = match AtomRegistry::load_cached(&atom_dir) {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(
                session_id = %session.id,
                error = %e,
                "rebuild_dag: AtomRegistry::load_cached failed; falling back to legacy taxonomy build"
            );
            return None;
        }
    };
    let archetypes = match ArchetypeRegistry::load_cached(&archetype_dir) {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(
                session_id = %session.id,
                error = %e,
                "rebuild_dag: ArchetypeRegistry::load_cached failed; falling back to legacy taxonomy build"
            );
            return None;
        }
    };

    // Overlay any Promoted proposals onto the base atom registry so the
    // v4 planner discovers them as legitimate candidates during
    // forward/backward search. Without this, the planner only sees
    // YAML-loaded atoms and the promoted node never enters
    // `workflow_dag.nodes`; the re-injection pass in `rebuild_dag` then
    // has to bolt it on after the fact, but since that runs AFTER
    // `build_dag_from_workflow_dag` already lowered to `session.dag`, the
    // promoted node never reaches `session.dag.tasks` and `emit_package`
    // drops it from WORKFLOW.json. The overlay lets the planner edge the
    // node in via its port contracts so the canonical lowering pass picks
    // it up.
    //
    // Variable shadow: the rest of `try_build_via_composer` reads
    // `atoms` unchanged. Non-promoted proposals contribute nothing
    // (the helper returns `None`); when no proposals are Promoted,
    // the overlay is empty and the returned registry is a clone
    // equivalent to the original.
    let promoted_overlay: Vec<ecaa_workflow_core::atom::AtomDefinition> = session
        .proposals
        .values()
        .filter_map(ecaa_workflow_core::hypothesized_proposal::promoted_proposal_to_atom_definition)
        .collect();
    let atoms = atoms.with_promoted_overlay(promoted_overlay);

    // The classifier usually populates `goal` from
    // modality-keywords.yaml. For explicit multi-omics prose, though,
    // SMEs often say "convergence", "overlap", or "what differs"
    // without using a canonical goal phrase. And for single-modality
    // bare prose ("scRNA-seq from human IVD samples with 10x
    // Chromium"), the SME may name the modality without a canonical
    // goal phrase. When the classifier didn't author a goal, infer one
    // from the primary archetype for the requested modality
    // (single-modality) or from the matching cross-omics archetype
    // (multi-modality). Single-modality bare prose previously fell
    // through to `build_dag_from_taxonomy`; with the legacy YAMLs
    // retired, archetype-driven inference is the only path.
    let cross_omics_goal = if modalities.len() >= 2 {
        infer_exact_cross_omics_goal(
            &archetypes,
            project_class_str,
            &modalities,
            &classification.intake_text,
        )
    } else {
        None
    };
    let mut goal: GoalSpec = cross_omics_goal.or_else(|| {
        classification.goal.clone().or_else(|| {
            infer_goal_for_modalities(
                &archetypes,
                project_class_str,
                &modalities,
                &classification.intake_text,
            )
        })
    })?;

    // Paired-protocol negation guard. Prompts like flex-scatac
    // ("DIFFERENT from SHARE-seq (no paired RNA)") mention share-seq /
    // multiome substrings inside a negative-context phrase. Naive
    // substring scans in the integrator / protocol / slot loops below
    // would resolve those substrings into goal.modifiers and route the
    // session to a paired-protocol archetype with forbidden atoms
    // (share_seq_barcode_match, multiome_arc_demultiplex). Mirror the
    // negation patterns already used in the novel-method override
    // guard above; share-seq / multiome canonicals are skipped in
    // every paired-protocol token scan when this flag is set.
    let paired_protocol_negated = {
        let l = classification.intake_text.to_lowercase();
        l.contains("different from share-seq")
            || l.contains("different from shareseq")
            || l.contains("different from multiome")
            || l.contains("not share-seq")
            || l.contains("not shareseq")
            || l.contains("not multiome")
            || l.contains("no paired rna")
            || l.contains("no paired")
            || l.contains("without share-seq")
            || l.contains("without multiome")
    };

    // Named-integrator discrimination. The keyword-path
    // `extract_goal` injects `modifiers["integrator"]` only when a
    // `goal_patterns:` phrase matched. For cross-omics scenarios the
    // SME's prose often names DIABLO / MOFA / SNF / WNN / share-seq /
    // multiome without any of those goal phrases (e.g. "DIABLO is the
    // named method" + "sparse-PLS-DA integrator"); the inferred goal
    // path then leaves `modifiers["integrator"]` unset, four cross-omics
    // archetypes covering `[bulk_rnaseq, proteomics]` tie on score,
    // and the composer routes alphabetically to the generic variant
    // — dropping the integrator-specific atoms. Re-run the integrator
    // scan against the full `intake_text` here so the v4 hoist
    // (`try_cross_omics_archetype_seed`) and the legacy dispatch
    // hoist both see the SME's actual method choice.
    if !goal.modifiers.contains_key("integrator") {
        const INTEGRATOR_TOKENS: &[(&str, &str)] = &[
            ("diablo", "diablo"),
            ("spls-da", "diablo"),
            ("spls da", "diablo"),
            ("sparse pls-da", "diablo"),
            ("sparse pls da", "diablo"),
            ("mixomics", "diablo"),
            ("mofa", "mofa"),
            ("factor decomposition", "mofa"),
            ("factor analysis", "mofa"),
            ("multi-omics factor", "mofa"),
            ("multi omics factor", "mofa"),
            ("snf integration", "snf"),
            ("similarity network fusion", "snf"),
            ("similarity-network fusion", "snf"),
            ("snftool", "snf"),
            ("wnn integration", "multiome"),
            ("weighted nearest neighbor", "multiome"),
            ("weighted-nearest neighbor", "multiome"),
            ("multiome arc", "multiome"),
            ("multiome", "multiome"),
            ("share seq", "share_seq"),
            ("shareseq", "share_seq"),
            ("share-seq", "share_seq"),
        ];
        // Use the classifier's normalize_for_match so prose with
        // hyphens, underscores, or mixed-case maps cleanly to the
        // token list (e.g., "Similarity-Network Fusion" → "similarity
        // network fusion"). Without normalization, hyphenated SME
        // prose missed token matches.
        let prose_norm =
            ecaa_workflow_core::classify::normalize_for_match(&classification.intake_text);
        for (phrase, canonical) in INTEGRATOR_TOKENS {
            let needle = ecaa_workflow_core::classify::normalize_for_match(phrase);
            if !needle.is_empty() && prose_norm.contains(&needle) {
                if paired_protocol_negated && matches!(*canonical, "share_seq" | "multiome") {
                    continue;
                }
                goal.modifiers
                    .insert("integrator".to_string(), (*canonical).to_string());
                break;
            }
        }
    }

    // Symmetric `protocol` slot discrimination. The slot-filling
    // architecture introduced a `protocol` slot on single_cell_de
    // (perturb_seq / generic) and on cross_omics_rnaseq_atac
    // (multiome_arc / share_seq / generic). The slot's `resolve_slot_value`
    // call inside the v4 planner only sees `goal.source_prose` (the
    // matched goal phrase, e.g. "differential expression") plus existing
    // `goal.modifiers` values — NOT the full intake prose. Without this
    // pre-population, Perturb-seq / SHARE-seq / multiome prompts route
    // to the `generic` slot value and miss the protocol-specific atoms
    // (sgrna_assignment, share_seq_barcode_match, multiome_arc_demultiplex).
    if !goal.modifiers.contains_key("protocol") {
        const PROTOCOL_TOKENS: &[(&str, &str)] = &[
            ("perturb-seq", "perturb_seq"),
            ("perturb seq", "perturb_seq"),
            ("perturbseq", "perturb_seq"),
            ("cas9 knockout screen", "perturb_seq"),
            ("crispri screen", "perturb_seq"),
            ("crispra screen", "perturb_seq"),
            ("feature barcoding", "perturb_seq"),
            ("feature-barcoding", "perturb_seq"),
            ("sgrna library", "perturb_seq"),
            ("guide library", "perturb_seq"),
            ("guide barcode", "perturb_seq"),
            ("single-cell crispr", "perturb_seq"),
            ("sgrna", "perturb_seq"),
            ("share-seq", "share_seq"),
            ("share seq", "share_seq"),
            ("shareseq", "share_seq"),
            ("multiome arc", "multiome_arc"),
            ("10x multiome", "multiome_arc"),
            ("cell ranger arc", "multiome_arc"),
            ("multiome", "multiome_arc"),
        ];
        let prose_norm =
            ecaa_workflow_core::classify::normalize_for_match(&classification.intake_text);
        for (phrase, canonical) in PROTOCOL_TOKENS {
            let needle = ecaa_workflow_core::classify::normalize_for_match(phrase);
            if !needle.is_empty() && prose_norm.contains(&needle) {
                if paired_protocol_negated && matches!(*canonical, "share_seq" | "multiome_arc") {
                    continue;
                }
                goal.modifiers
                    .insert("protocol".to_string(), (*canonical).to_string());
                break;
            }
        }
    }

    // N-way intent propagation. classify::is_n_way_intent reads
    // intake_text + N_WAY_MARKERS to detect "three way analysis",
    // "tri-omics", or comma-list of ≥3 distinct modality nouns. The
    // v4 planner does its own n_way detection later but only sees
    // `goal.source_prose + modifiers.values()` (a fragment), NOT the
    // full intake_text. Plumb the signal through goal.modifiers
    // ["n_way_intent"] — the planner's prose reconstruction then
    // sees a strong-marker value and `find_match_cross_omics(n_way:
    // true)` unlocks subset matching for 3-way archetypes.
    if ecaa_workflow_core::classify::is_n_way_intent(&classification.intake_text) {
        goal.modifiers
            .entry("n_way_intent".to_string())
            .or_insert_with(|| "three way analysis".to_string());
    }

    // The v4 planner's cross-omics and slot matchers need the full
    // SME prose, not just the short classifier goal phrase ("peak
    // calling", "differential expression"). Preserve the canonical
    // EDAM goal fields, but let downstream prose-aware matching see
    // every named modality and protocol.
    if !classification.intake_text.trim().is_empty() {
        goal.source_prose = Some(classification.intake_text.clone());
    }

    // Single-source-of-truth slot-keyword scan. The hardcoded
    // INTEGRATOR_TOKENS / PROTOCOL_TOKENS lists above are a working
    // copy of the canonical lists that live in each slot manifest's
    // `keywords:` field (config/archetypes/<id>.slots.yaml). Walk every
    // archetype with a SlotManifest and let `resolve_slot_value` pick
    // a value from intake_text. If the slot's canonical default isn't
    // chosen (i.e., the prose matched a non-default value's keyword
    // list), inject that value into goal.modifiers. This catches slot
    // values whose keyword lists evolved in YAML but the hardcoded
    // lists above didn't track. Single source of truth: the slot YAML.
    for (_id, archetype) in archetypes.iter() {
        let Some(slots) = &archetype.slots else {
            continue;
        };
        if goal.modifiers.contains_key(&slots.slot_name) {
            continue;
        }
        let chosen = ecaa_workflow_core::archetype_slots::resolve_slot_value(
            slots,
            &classification.intake_text,
        );
        if chosen != slots.default {
            // Negation guard mirrors the integrator/protocol scans
            // above. `resolve_slot_value` does a flat substring scan
            // over slot keywords against intake_text; a phrase like
            // "DIFFERENT from SHARE-seq (no paired RNA)" otherwise
            // resolves protocol=share_seq / integrator=share_seq /
            // integrator=multiome and routes the session to a
            // paired-protocol archetype.
            let is_paired_canonical = match slots.slot_name.as_str() {
                "protocol" => matches!(chosen.as_str(), "share_seq" | "multiome_arc"),
                "integrator" => matches!(chosen.as_str(), "share_seq" | "multiome"),
                _ => false,
            };
            if paired_protocol_negated && is_paired_canonical {
                tracing::debug!(
                    session_id = %session.id,
                    archetype = %archetype.id,
                    slot = %slots.slot_name,
                    value = %chosen,
                    "slot_value_skipped_under_negation_context"
                );
                continue;
            }
            tracing::debug!(
                session_id = %session.id,
                archetype = %archetype.id,
                slot = %slots.slot_name,
                value = %chosen,
                "slot_value_resolved_from_intake_prose"
            );
            goal.modifiers.insert(slots.slot_name.clone(), chosen);
        }
    }

    // Force-add companion modality for paired-protocol scenarios.
    // When a `protocol` slot value implies a multi-modality archetype
    // (share_seq + multiome_arc both require single_cell_rnaseq +
    // atac_seq coverage), the cross-omics matcher must see BOTH
    // modalities in `modalities`. Classifier under-counting (atac_seq
    // keywords with 1-2 hits failing the IDF + threshold gates)
    // routinely drops the companion. Force-add it here when the
    // protocol value names a paired protocol, regardless of classifier
    // score. Without this, share-seq/multiome prompts route to the
    // single-modality `single_cell_de` archetype (with maybe a slot
    // expansion that adds barcode_match / demultiplex atoms but
    // misses joint_wnn_integration + peak_to_gene_linking).
    {
        let needs_paired_sc_atac = matches!(
            goal.modifiers.get("protocol").map(String::as_str),
            Some("share_seq") | Some("multiome_arc")
        );
        if needs_paired_sc_atac {
            // Sanity gate: if classifier produced chip_seq AND the
            // prose names tri-omics explicitly (three-way analysis,
            // tri-omics, RNA+ATAC+ChIP comma-list), the protocol slot
            // likely fired off an incidental "multiome" mention in a
            // bulk tri-omics scenario. Skip canonical force in that
            // case; let the subset matcher pick the 3-way archetype.
            // For share-seq/multiome prompts, the protocol slot is the
            // STRONGER signal — those prompts may incidentally mention
            // chromatin / TF-binding which IDF mis-routes to chip_seq.
            let intake_lower = classification.intake_text.to_lowercase();
            let n_way_in_prose = intake_lower.contains("tri-omics")
                || intake_lower.contains("tri omics")
                || intake_lower.contains("three-way")
                || intake_lower.contains("three way analysis")
                || intake_lower.contains("trio analysis");
            let looks_like_tri_omics = (modalities.contains(&"chip_seq")
                || modalities.contains(&"variant_calling"))
                && n_way_in_prose;
            if looks_like_tri_omics {
                tracing::warn!(
                    session_id = %session.id,
                    protocol = %goal.modifiers.get("protocol").map(String::as_str).unwrap_or(""),
                    prior_modalities = ?modalities,
                    "skipping_canonical_paired_sc_atac_force_due_to_tri_omics_signal",
                );
            } else {
                // For share_seq / multiome_arc, the canonical modality
                // pair is ALWAYS [single_cell_rnaseq, atac_seq]. These
                // protocols are single-cell by definition; any classifier
                // route that picked bulk_rnaseq (e.g., "RNA-seq" generic
                // keywords overrode "snRNA" specific ones) is wrong.
                // Replace `modalities` with the canonical pair so the
                // cross-omics matcher sees exactly the right set.
                tracing::warn!(
                    session_id = %session.id,
                    protocol = %goal.modifiers.get("protocol").map(String::as_str).unwrap_or(""),
                    prior_modalities = ?modalities,
                    "forcing_canonical_paired_sc_atac_modalities",
                );
                modalities = vec!["single_cell_rnaseq", "atac_seq"];
            }
        }
        // Symmetric for integrator-driven cross-omics: DIABLO / MOFA /
        // SNF imply RNA + proteomics (or another pair). The integrator
        // value names the method but doesn't disambiguate modality
        // pair. For now, when integrator ∈ {diablo, mofa, snf} AND the
        // classifier picked proteomics OR bulk_rnaseq alone, force-add
        // the companion of the OTHER side. The cross-omics matcher
        // then sees [bulk_rnaseq, proteomics] and picks
        // cross_omics_rnaseq_proteomics base + slot.
        let integrator = goal.modifiers.get("integrator").map(String::as_str);
        let needs_proteomics_pair =
            matches!(integrator, Some("diablo") | Some("mofa") | Some("snf"));
        if needs_proteomics_pair {
            let has_bulk_rna = modalities.contains(&"bulk_rnaseq");
            let has_proteo = modalities.contains(&"proteomics");
            if has_bulk_rna && !has_proteo {
                tracing::warn!(
                    session_id = %session.id,
                    integrator = %integrator.unwrap_or(""),
                    "force_adding_proteomics_companion_for_named_integrator",
                );
                modalities.push("proteomics");
            } else if has_proteo && !has_bulk_rna {
                tracing::warn!(
                    session_id = %session.id,
                    integrator = %integrator.unwrap_or(""),
                    "force_adding_bulk_rnaseq_companion_for_named_integrator",
                );
                modalities.push("bulk_rnaseq");
            }
        }
    }

    // `compose_with_version_and_modalities_full` returns
    // a `ComposerOutput` carrying the legacy `CompositionResult` plus
    // (for v4 sessions only) the typed `WorkflowDag`, ranked
    // alternatives, `ComposeOutcome`, and per-node policy decisions.
    // Non-v4 sessions get a `ComposerOutput::legacy(composition)`
    // wrapper so the call site is uniform.
    //
    // Thread the session's active policy bundle (if any)
    // into the composer so per-node policy gate fires at compose time.
    // The bundle id is set by `POST /api/chat/session/:id/policy-bundle`
    // (typically wired via ClinicalConfirmGate for clinical sessions).
    let policy_ctx_owned = session
        .active_policy_bundle
        .as_deref()
        .and_then(resolve_policy_bundle);

    // R1/R2 closure (closure-residuals plan Task 1.4) — wire the
    // cross-session opaque-type observation sink from the session's
    // runtime directory. `ECAA_CHAT_SESSIONS_DIR` mirrors the
    // `SessionStore` default (`~/.ecaa-workflow/sessions`); the
    // aggregator lives at `<sessions_dir>/<session_id>/_opaque_registry.jsonl`.
    // Bare callers (CLI `intake`, eval-baselines, tests) pass `None,
    // None` and preserve existing log-only behavior.
    let sessions_dir = std::env::var("ECAA_CHAT_SESSIONS_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| {
            std::env::var("HOME")
                .map(|h| std::path::PathBuf::from(h).join(".ecaa-workflow/sessions"))
                .unwrap_or_else(|_| std::path::PathBuf::from(".ecaa-workflow/sessions"))
        });
    let aggregator_path = sessions_dir
        .join(session.id.to_string())
        .join("_opaque_registry.jsonl");
    let aggregator = std::sync::Arc::new(crate::session::opaque_aggregator::OpaqueAggregator::new(
        aggregator_path,
    ));
    let opaque_sink: std::sync::Arc<
        dyn ecaa_workflow_core::compatibility::engine::OpaqueObservationSink + Send + Sync,
    > = std::sync::Arc::new(
        crate::session::opaque_aggregator::OpaqueObservationSinkImpl::new(aggregator),
    );
    let session_id_str = session.id.to_string();

    let output = match compose_with_version_and_modalities_full(
        &goal,
        project_class_str,
        &atoms,
        &archetypes,
        session.composer_version,
        &modalities,
        policy_ctx_owned.as_ref(),
        Some(opaque_sink),
        Some(session_id_str.as_str()),
    ) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(
                session_id = %session.id,
                error = ?e,
                "rebuild_dag: compose_with_version returned error; falling back to legacy taxonomy build"
            );
            return None;
        }
    };
    let composition = &output.composition;
    if modalities.len() >= 2
        && !composition
            .matched_archetype
            .as_deref()
            .map(|id| id.starts_with("cross_omics_"))
            .unwrap_or(false)
    {
        // Degraded path: cross-omics request couldn't resolve to a
        // matching cross-omics archetype (set-equality on the matcher
        // failed, typically when the classifier produced only 2 of 3
        // requested modalities, or when no archetype declares the
        // requested modality set). The degraded path replaces an
        // earlier branch that returned None, which caused `session.dag`
        // to stay empty,
        // `append_intake_prose` to error with PreconditionFailure, and
        // the LLM to either retry indefinitely or proceed to
        // `emit_package` which then refused with "no DAG built". The
        // session would enter Blocked(HostError) and never recover.
        // Accept the single-modality archetype as a degraded
        // outcome — the SME sees a single-modality DAG with the
        // matched archetype's atoms, can amend it, and at least gets
        // a runnable workflow instead of a hard block.
        tracing::warn!(
            session_id = %session.id,
            matched_archetype = ?composition.matched_archetype,
            requested_modalities = ?modalities,
            "rebuild_dag: cross-omics request degraded to single-modality archetype (cross-omics matcher returned no set-equality match)"
        );
    }

    // v4 sessions route through the canonical `WorkflowDag → DAG`
    // lowering pass (build_dag_from_workflow_dag). Falls through to
    // `build_dag_from_composition` only when the v4 planner did not
    // produce a typed `WorkflowDag` (today: never on the success path,
    // but the fallback is preserved as a safety net so a regression in
    // v4 lowering doesn't break composition entirely).
    let methods = session.intake_methods.to_core();
    let dag_result = if let Some(workflow_dag) = output.workflow_dag.as_ref() {
        tracing::debug!(
            session_id = %session.id,
            composer_version = session.composer_version,
            "rebuild_dag: v4 path — lowering WorkflowDag through build_dag_from_workflow_dag"
        );
        build_dag_from_workflow_dag(workflow_dag, &workflow_id(&session.id))
    } else {
        // Cross-dependencies aren't tracked on `CompositionResult`
        // today (the archetype's `cross_dependencies` block is read
        // off the pinned snapshot directly). Pass empty for now; if
        // future archetypes add cross-deps this is the wiring point.
        build_dag_from_composition(composition, &workflow_id(&session.id), &methods, &[])
    };
    match dag_result {
        Ok(mut dag) => {
            let mut workflow_dag_for_session = output.workflow_dag.clone();
            // Post-build intake-fact gates. v4 dispatch does not yet
            // thread `IntakeContext` through `compose_v4_dispatch_full`,
            // so the literature opt-in gate that `compose_with_intake`
            // applies in slot_fill is bypassed for v4 sessions. Until
            // the v4 planner takes intake facts as input, apply the
            // same drop semantics here.
            //
            // The default semantics match slot_fill::compose_with_intake:
            //   * `literature_review_requested = false` (default) →
            //      drop `review_prior_work` + `contextualize_findings_*`
            //      + their `validate_*` siblings. Surviving tasks lose
            //      their `depends_on` edges to the dropped tasks.
            //
            // Additionally apply the counts-only-input gate when the
            // SME-supplied inputs declare counts-matrix only (no FASTQ
            // upstream) — FASTQ-level atoms are unreachable in that
            // case and the composer's archetype catalog still includes
            // them.
            let dropped_lit = apply_literature_opt_in_gate(&mut dag);
            let dropped_fastq = apply_counts_only_input_gate(&mut dag, session);
            if let Some(wf) = workflow_dag_for_session.as_mut() {
                use ecaa_workflow_core::composer::LITERATURE_OPT_IN_ATOM_IDS;
                let wf_dropped_lit =
                    prune_workflow_dag_roots_with_companions(wf, LITERATURE_OPT_IN_ATOM_IDS);
                let wf_dropped_fastq = prune_counts_only_input_workflow_dag(wf, session);
                if !wf_dropped_lit.is_empty() || !wf_dropped_fastq.is_empty() {
                    tracing::debug!(
                        session_id = %session.id,
                        workflow_dag_dropped_literature = ?wf_dropped_lit,
                        workflow_dag_dropped_fastq_atoms = ?wf_dropped_fastq,
                        "rebuild_dag: applied intake-fact post-filters to authoritative WorkflowDag"
                    );
                }
            }
            if !dropped_lit.is_empty() || !dropped_fastq.is_empty() {
                tracing::info!(
                    session_id = %session.id,
                    dropped_literature = ?dropped_lit,
                    dropped_fastq_atoms = ?dropped_fastq,
                    "rebuild_dag: applied intake-fact post-filters to v4 DAG"
                );
            }
            // V3 cascade-invalidate any prior assumption
            // resolutions whose upstream contracts changed. The
            // "any → unresolved" reset edge closes the §15.1 state
            // machine: an upstream contract change must re-open
            // downstream resolutions so the SME re-confirms with
            // the new context.
            //
            // Runs BEFORE the cache update so we can compare prior
            // vs new workflow_dag. Idempotent — comparing the same
            // dag against itself produces no decision records.
            // Clone the prior dag so the immutable borrow ends before
            // we mutably borrow `session` to record decisions.
            let prior_dag = session.workflow_dag.clone();
            if let (Some(prior), Some(new)) =
                (prior_dag.as_ref(), workflow_dag_for_session.as_ref())
            {
                let mut new_owned = new.clone();
                cascade_invalidate_assumptions(prior, &mut new_owned, session);
                // v3 P8 — five additional non-monotonic lifecycle edges.
                detect_lifecycle_adversarial_edges(Some(prior), &new_owned, session);
                session.workflow_dag = Some(new_owned);
            } else {
                if let Some(new) = workflow_dag_for_session.as_ref() {
                    detect_lifecycle_adversarial_edges(None, new, session);
                }
                session.workflow_dag = workflow_dag_for_session;
            }
            session.compose_outcome = output.compose_outcome.clone();
            session.ranked_alternatives = output.ranked_alternatives.clone();
            session.policy_decisions = output.policy_decisions.clone();
            // Thread the planner's matched archetype back into the
            // classification so `ro_crate::p_plan_entity` emits a
            // non-null `matchedArchetype` field. Without this, the
            // `set_intake_modality` handler clears `archetype_id` to
            // None (correct, since the prior modality's archetype is
            // stale) but nothing re-populates it after `rebuild_dag`
            // runs the v4 planner — so every emitted RO-Crate carries
            // `matchedArchetype: null` regardless of which archetype
            // the planner selected. ECAA D7 invariants depend on this
            // field being non-null.
            if let Some(archetype) = composition.matched_archetype.as_deref() {
                if let Some(c) = session.classification.as_mut() {
                    c.archetype_id = Some(archetype.to_string());
                }
            }
            Some(dag)
        }
        Err(e) => {
            tracing::warn!(
                session_id = %session.id,
                error = %e,
                "rebuild_dag: DAG build failed; falling back to legacy taxonomy build"
            );
            None
        }
    }
}

/// Literature opt-in gate post-filter. Applies the same drop
/// semantics as `slot_fill::compose_with_intake`'s literature gate to
/// the v4 DAG, since the v4 planner does not yet thread `IntakeContext`
/// through `compose_v4_dispatch_full`.
///
/// Drops `review_prior_work` and `contextualize_findings_with_literature`
/// (plus their `validate_*` siblings) when the session's intake facts
/// indicate `literature_review_requested == false` (the default).
/// Surviving tasks lose any `depends_on` edges to the dropped tasks —
/// `reporting`'s dependency on `contextualize_findings_with_literature`
/// becomes a no-op edge that the DAG drops automatically.
///
/// Returns the actually-dropped task ids (filtered to ones that
/// existed) for audit logging.
fn apply_literature_opt_in_gate(
    dag: &mut ecaa_workflow_core::dag::DAG,
) -> std::collections::BTreeSet<ecaa_workflow_core::dag::TaskId> {
    // Until v4 threads intake_facts through, default to opt-out
    // (matching slot_fill's default behavior — review_prior_work only
    // surfaces when the SME explicitly opts in). The single source of
    // truth for which atoms are "literature opt-in" lives in
    // `composer::slot_fill::LITERATURE_OPT_IN_ATOM_IDS`; mirror its set
    // here to keep the gate behavior consistent across composer paths.
    use ecaa_workflow_core::composer::LITERATURE_OPT_IN_ATOM_IDS;
    let present: Vec<&str> = LITERATURE_OPT_IN_ATOM_IDS
        .iter()
        .filter(|id| dag.tasks.contains_key(**id))
        .copied()
        .collect();
    if present.is_empty() {
        return std::collections::BTreeSet::new();
    }
    let dropped = dag.drop_tasks_with_validators(&present);
    // Only return ids that were actually present in the DAG.
    dropped
        .into_iter()
        .filter(|id| {
            present.contains(&id.as_str())
                || present
                    .iter()
                    .any(|p| format!("validate_{p}").as_str() == id.as_str())
        })
        .collect()
}

fn companion_base_id(task_id: &str) -> &str {
    task_id
        .strip_prefix("discover_")
        .or_else(|| task_id.strip_prefix("validate_"))
        .unwrap_or(task_id)
}

/// Drop root task nodes plus their `discover_*` / `validate_*`
/// companions from the authoritative typed `WorkflowDag`.
///
/// This mirrors `DAG::drop_tasks_with_validators` for the v4 source of
/// truth. The lowered `session.dag` cache is allowed to be rebuilt at
/// any point, so intake-fact gates must also remove nodes and edges
/// from `session.workflow_dag`; otherwise gated tasks reappear during
/// emit-time lowering.
///
/// Splices `(parent → child)` edges across each dropped node so that a
/// chain-middle drop (e.g. the counts-only-input gate stripping the
/// FASTQ run `data_acquisition → raw_qc → … → quantification →
/// qc_preprocessing`) does not leave the downstream consumer with an
/// empty `depends_on` list. The splice walks transitively through
/// consecutive dropped nodes so multi-atom drops collapse to a single
/// surviving-parent → surviving-child edge. Validators are excluded
/// from being promoted to data sources during splicing.
fn prune_workflow_dag_roots_with_companions(
    workflow_dag: &mut ecaa_workflow_core::workflow_contracts::task_node::WorkflowDag,
    roots: &[&str],
) -> std::collections::BTreeSet<String> {
    let root_ids: std::collections::BTreeSet<&str> = roots.iter().copied().collect();
    if root_ids.is_empty() {
        return std::collections::BTreeSet::new();
    }

    let dropped: std::collections::BTreeSet<String> = workflow_dag
        .nodes
        .iter()
        .filter_map(|node| {
            let base = companion_base_id(node.id.as_str());
            if root_ids.contains(node.id.as_str()) || root_ids.contains(base) {
                Some(node.id.clone())
            } else {
                None
            }
        })
        .collect();
    if dropped.is_empty() {
        return dropped;
    }

    // Capture (parents, children) per dropped node BEFORE we tear
    // edges down. Include dropped relatives — the transitive walk
    // needs them as hops to find the surviving boundary. Validator
    // filtering happens inside the helpers, not at capture time. We
    // resolve edge endpoints to atom ids by stripping the trailing
    // `__<port>` qualifier that the v4 composer puts on edge endpoint
    // identifiers.
    let parents_of: std::collections::BTreeMap<String, Vec<String>> = dropped
        .iter()
        .map(|d| {
            let parents: Vec<String> = workflow_dag
                .edges
                .iter()
                .filter(|e| edge_node_id(&e.to_node) == d.as_str())
                .map(|e| edge_node_id(&e.from_node).to_string())
                .collect();
            (d.clone(), parents)
        })
        .collect();
    let children_of: std::collections::BTreeMap<String, Vec<String>> = dropped
        .iter()
        .map(|d| {
            let children: Vec<String> = workflow_dag
                .edges
                .iter()
                .filter(|e| edge_node_id(&e.from_node) == d.as_str())
                .map(|e| edge_node_id(&e.to_node).to_string())
                .collect();
            (d.clone(), children)
        })
        .collect();

    workflow_dag
        .nodes
        .retain(|node| !dropped.contains(&node.id));
    workflow_dag.edges.retain(|edge| {
        !dropped.contains(edge_node_id(&edge.from_node))
            && !dropped.contains(edge_node_id(&edge.to_node))
    });
    for assumption in &mut workflow_dag.assumptions.entries {
        assumption
            .affects_nodes
            .retain(|node_id| !dropped.contains(node_id));
    }

    // Splice surviving (parent → child) edges. Walk consecutive
    // dropped nodes transitively in both directions so multi-atom
    // runs collapse to a single bridging edge.
    use ecaa_workflow_core::workflow_contracts::edge::EdgeContract;
    let mut spliced_edges: Vec<EdgeContract> = Vec::new();
    for d in &dropped {
        let parents = transitive_surviving_workflow_parents(d, &dropped, &parents_of);
        let children = transitive_surviving_workflow_children(d, &dropped, &children_of);
        for parent in &parents {
            for child in &children {
                if parent == child {
                    continue;
                }
                let already_present = workflow_dag.edges.iter().any(|e| {
                    edge_node_id(&e.from_node) == parent.as_str()
                        && edge_node_id(&e.to_node) == child.as_str()
                }) || spliced_edges.iter().any(|e| {
                    edge_node_id(&e.from_node) == parent.as_str()
                        && edge_node_id(&e.to_node) == child.as_str()
                });
                if !already_present {
                    spliced_edges.push(EdgeContract::synthetic_splice(
                        parent.clone(),
                        child.clone(),
                    ));
                }
            }
        }
    }
    workflow_dag.edges.extend(spliced_edges);
    dropped
}

/// Endpoint identifiers on `EdgeContract` carry an optional `__<port>`
/// suffix in some composer outputs. Strip it to recover the atom id.
fn edge_node_id(endpoint: &str) -> &str {
    endpoint.split("__").next().unwrap_or(endpoint)
}

fn transitive_surviving_workflow_parents(
    start: &str,
    drop_set: &std::collections::BTreeSet<String>,
    parents_of: &std::collections::BTreeMap<String, Vec<String>>,
) -> std::collections::BTreeSet<String> {
    let mut out: std::collections::BTreeSet<String> = Default::default();
    let mut seen: std::collections::BTreeSet<String> = Default::default();
    let mut stack: Vec<String> = vec![start.to_string()];
    while let Some(node) = stack.pop() {
        if !seen.insert(node.clone()) {
            continue;
        }
        if let Some(parents) = parents_of.get(&node) {
            for p in parents {
                if drop_set.contains(p) {
                    stack.push(p.clone());
                } else if !p.starts_with("validate_") {
                    out.insert(p.clone());
                }
            }
        }
    }
    out
}

fn transitive_surviving_workflow_children(
    start: &str,
    drop_set: &std::collections::BTreeSet<String>,
    children_of: &std::collections::BTreeMap<String, Vec<String>>,
) -> std::collections::BTreeSet<String> {
    let mut out: std::collections::BTreeSet<String> = Default::default();
    let mut seen: std::collections::BTreeSet<String> = Default::default();
    let mut stack: Vec<String> = vec![start.to_string()];
    while let Some(node) = stack.pop() {
        if !seen.insert(node.clone()) {
            continue;
        }
        if let Some(children) = children_of.get(&node) {
            for c in children {
                if drop_set.contains(c) {
                    stack.push(c.clone());
                } else if !c.starts_with("validate_") {
                    out.insert(c.clone());
                }
            }
        }
    }
    out
}

const FASTQ_LEVEL_ATOM_IDS: &[&str] =
    &["raw_qc", "sequence_trimming", "alignment", "quantification"];

fn counts_only_inputs(session: &crate::session::Session) -> bool {
    if session.inputs.is_empty() {
        return false;
    }
    let any_fastq = session.inputs.iter().any(|i| {
        i.files.iter().any(|f| {
            let lower = f.relpath.to_ascii_lowercase();
            lower.ends_with(".fastq")
                || lower.ends_with(".fastq.gz")
                || lower.ends_with(".fq")
                || lower.ends_with(".fq.gz")
        })
    });
    !any_fastq
}

/// True when the SME has signalled counts-level entry by excluding the
/// read→counts bridge atoms. `quantification` produces a counts matrix
/// from alignments and `alignment` produces those alignments from reads;
/// excluding either means no counts can be derived from raw reads in this
/// workflow, so the SME is supplying counts directly. That makes the
/// upstream FASTQ-level atoms (`raw_qc`, `sequence_trimming`) vestigial —
/// exactly as a registered counts-matrix input would, and independent of
/// whether the SME registered any input file. This catches the common
/// path where the SME declares counts-only in prose and the LLM excludes
/// the read-level chain via `set_intake_excluded_atoms` without ever
/// touching the Inputs tab.
fn excludes_read_to_counts_bridge(session: &crate::session::Session) -> bool {
    session
        .excluded_atoms
        .iter()
        .any(|a| a == "quantification" || a == "alignment")
}

/// Counts-level entry: the registered inputs are counts-only (no FASTQ),
/// OR the SME explicitly excluded the read→counts bridge. Either way the
/// FASTQ-level atoms are unreachable and must be pruned so a leftover
/// `raw_qc` / `sequence_trimming` doesn't get absorbed into
/// `reporting.depends_on` by the lowering pass's orphan-strand repair.
fn counts_level_entry(session: &crate::session::Session) -> bool {
    counts_only_inputs(session) || excludes_read_to_counts_bridge(session)
}

fn prune_counts_only_input_workflow_dag(
    workflow_dag: &mut ecaa_workflow_core::workflow_contracts::task_node::WorkflowDag,
    session: &crate::session::Session,
) -> std::collections::BTreeSet<String> {
    if !counts_level_entry(session) {
        return std::collections::BTreeSet::new();
    }
    prune_workflow_dag_roots_with_companions(workflow_dag, FASTQ_LEVEL_ATOM_IDS)
}

/// Counts-only-input gate post-filter. When the SME-registered inputs
/// declare a counts-matrix shape (no FASTQ files in the registered
/// roots), FASTQ-level atoms — `raw_qc`, `sequence_trimming`,
/// `alignment`, `quantification` — are unreachable and the composer's
/// archetype catalog includes them anyway. Drop them and their
/// `validate_*` siblings so the DAG is consistent with the input
/// declaration.
///
/// Heuristic for "counts-only": no registered input file has a FASTQ
/// extension (`.fastq`, `.fastq.gz`, `.fq`, `.fq.gz`). When inputs are
/// absent (legacy intake without a registered local path) the gate
/// does NOT fire — the SME may be operating against public-repo
/// accessions whose FASTQs the data_acquisition stage will materialize.
fn apply_counts_only_input_gate(
    dag: &mut ecaa_workflow_core::dag::DAG,
    session: &crate::session::Session,
) -> std::collections::BTreeSet<ecaa_workflow_core::dag::TaskId> {
    if !counts_level_entry(session) {
        return std::collections::BTreeSet::new();
    }
    // No FASTQ in registered inputs → drop FASTQ-level atoms when
    // they're present in the DAG. The validate_* siblings are dropped
    // by `drop_tasks_with_validators` automatically.
    let present: Vec<&str> = FASTQ_LEVEL_ATOM_IDS
        .iter()
        .filter(|id| dag.tasks.contains_key(**id))
        .copied()
        .collect();
    if present.is_empty() {
        return std::collections::BTreeSet::new();
    }
    let dropped = dag.drop_tasks_with_validators(&present);
    dropped
        .into_iter()
        .filter(|id| {
            FASTQ_LEVEL_ATOM_IDS.contains(&id.as_str())
                || FASTQ_LEVEL_ATOM_IDS
                    .iter()
                    .any(|p| format!("validate_{p}").as_str() == id.as_str())
        })
        .collect()
}

/// V3 cascade-invalidate prior assumption resolutions when
/// the upstream contract under a previously-resolved assumption has
/// changed. Resets the resolution to `Unresolved` on the new DAG and
/// appends an `AssumptionInvalidated` decision-log entry so audit
/// replay can reconstruct the reset edge.
///
/// Closes the §15.1 state-machine's "any → Unresolved" edge for
/// upstream-contract changes. Conservative change detection: any
/// difference in `affects_nodes` OR `source` between prior and new
/// counts as an upstream contract change. Precise contract-graph
/// diffing is a future follow-up.
fn cascade_invalidate_assumptions(
    prior: &ecaa_workflow_core::workflow_contracts::task_node::WorkflowDag,
    new: &mut ecaa_workflow_core::workflow_contracts::task_node::WorkflowDag,
    session: &mut Session,
) {
    use ecaa_workflow_core::decision_log::{DecisionActor, DecisionType};
    use ecaa_workflow_core::workflow_contracts::evidence::AssumptionResolution;

    let mut invalidated: Vec<(String, String)> = Vec::new();
    for new_a in &mut new.assumptions.entries {
        if let Some(prior_a) = prior.assumptions.entries.iter().find(|p| p.id == new_a.id) {
            if !matches!(prior_a.resolution, AssumptionResolution::Unresolved)
                && contract_changed_under(prior_a, new_a)
            {
                let change = describe_assumption_change(prior_a, new_a);
                new_a.resolution = AssumptionResolution::Unresolved;
                invalidated.push((new_a.id.clone(), change));
            }
        }
    }
    // Decision-log writes are detached from the mutation to avoid
    // double-borrow on session.
    for (assumption_id, upstream_change) in invalidated {
        session.record_decision(
            DecisionType::AssumptionInvalidated {
                assumption_id,
                upstream_change,
            },
            DecisionActor::Harness,
            None,
        );
    }
}

/// V3 conservative upstream-contract change detection.
/// Returns true when either `affects_nodes` or `source` differs
/// between the prior and new assumption. Precise change detection
/// (port-shape diff, edge-set diff) lands later.
fn contract_changed_under(
    prior_a: &ecaa_workflow_core::workflow_contracts::evidence::Assumption,
    new_a: &ecaa_workflow_core::workflow_contracts::evidence::Assumption,
) -> bool {
    prior_a.affects_nodes != new_a.affects_nodes || prior_a.source != new_a.source
}

/// Short human-readable description of what changed under the
/// assumption between prior and new DAGs. Recorded on the
/// `AssumptionInvalidated` decision-log entry.
fn describe_assumption_change(
    prior_a: &ecaa_workflow_core::workflow_contracts::evidence::Assumption,
    new_a: &ecaa_workflow_core::workflow_contracts::evidence::Assumption,
) -> String {
    format!(
        "affects_nodes: {:?} → {:?}; source: {:?} → {:?}",
        prior_a.affects_nodes, new_a.affects_nodes, prior_a.source, new_a.source
    )
}

/// V3 detect the five non-monotonic lifecycle edges that
/// `cascade_invalidate_assumptions` does NOT already handle.
pub(crate) fn detect_lifecycle_adversarial_edges(
    prior: Option<&ecaa_workflow_core::workflow_contracts::task_node::WorkflowDag>,
    new: &ecaa_workflow_core::workflow_contracts::task_node::WorkflowDag,
    session: &mut Session,
) {
    use ecaa_workflow_core::lifecycle_adversarial::LifecycleTransition;

    let mut transitions: Vec<LifecycleTransition> = Vec::new();
    transitions.extend(detect_same_user_contradiction(&session.decisions));
    transitions.extend(detect_cross_user_conflict(&session.decisions));
    transitions.extend(detect_forbidden_waiver(&session.decisions));
    transitions.extend(detect_verifier_unresolvability(prior, new));
    transitions.extend(detect_production_revocation(prior, new));

    for transition in transitions {
        enqueue_adjudication(session, transition);
    }
}

pub(crate) fn detect_same_user_contradiction(
    decisions: &[ecaa_workflow_core::decision_log::DecisionRecord],
) -> Vec<ecaa_workflow_core::lifecycle_adversarial::LifecycleTransition> {
    use ecaa_workflow_core::decision_log::{DecisionActor, DecisionType};
    use ecaa_workflow_core::lifecycle_adversarial::LifecycleTransition;

    let mut by_actor_and_assumption: std::collections::BTreeMap<
        (String, String),
        Vec<(String, String)>,
    > = std::collections::BTreeMap::new();

    for d in decisions {
        if !matches!(d.actor, DecisionActor::Sme | DecisionActor::Llm) {
            continue;
        }
        if let DecisionType::AssumptionResolved { id, resolution } = &d.decision {
            let actor_label = match d.actor {
                DecisionActor::Sme => "sme",
                DecisionActor::Llm => "llm",
                DecisionActor::Harness => "harness",
            };
            by_actor_and_assumption
                .entry((actor_label.to_string(), id.clone()))
                .or_default()
                .push((d.timestamp.to_rfc3339(), resolution.clone()));
        }
    }

    let mut out = Vec::new();
    for ((actor, assumption_id), records) in by_actor_and_assumption.iter() {
        if records.len() < 2 {
            continue;
        }
        let last = &records[records.len() - 1];
        let prior = &records[records.len() - 2];
        if are_opposite_resolutions(&prior.1, &last.1) {
            out.push(LifecycleTransition::SameUserContradiction {
                actor: actor.clone(),
                assumption_id: assumption_id.clone(),
                prior_record_id: prior.0.clone(),
                new_record_id: last.0.clone(),
            });
        }
    }
    out
}

pub(crate) fn detect_cross_user_conflict(
    decisions: &[ecaa_workflow_core::decision_log::DecisionRecord],
) -> Vec<ecaa_workflow_core::lifecycle_adversarial::LifecycleTransition> {
    use ecaa_workflow_core::decision_log::{DecisionActor, DecisionType};
    use ecaa_workflow_core::lifecycle_adversarial::LifecycleTransition;

    let mut by_assumption: std::collections::BTreeMap<String, Vec<(String, String, String)>> =
        std::collections::BTreeMap::new();

    for d in decisions {
        let actor_label = match d.actor {
            DecisionActor::Sme => "sme",
            DecisionActor::Llm => "llm",
            DecisionActor::Harness => "harness",
        };
        if let DecisionType::AssumptionResolved { id, resolution } = &d.decision {
            by_assumption.entry(id.clone()).or_default().push((
                actor_label.to_string(),
                d.timestamp.to_rfc3339(),
                resolution.clone(),
            ));
        }
    }

    let mut out = Vec::new();
    for (assumption_id, records) in by_assumption.iter() {
        let mut conflicting_records: Vec<String> = Vec::new();
        let mut actor_a: Option<String> = None;
        let mut actor_b: Option<String> = None;
        'outer: for i in 0..records.len() {
            for j in (i + 1)..records.len() {
                if records[i].0 != records[j].0
                    && are_opposite_resolutions(&records[i].2, &records[j].2)
                {
                    actor_a = Some(records[i].0.clone());
                    actor_b = Some(records[j].0.clone());
                    conflicting_records.push(records[i].1.clone());
                    conflicting_records.push(records[j].1.clone());
                    break 'outer;
                }
            }
        }
        if let (Some(a), Some(b)) = (actor_a, actor_b) {
            out.push(LifecycleTransition::CrossUserConflict {
                actor_a: a,
                actor_b: b,
                assumption_id: assumption_id.clone(),
                records: conflicting_records,
            });
        }
    }
    out
}

pub(crate) fn detect_forbidden_waiver(
    decisions: &[ecaa_workflow_core::decision_log::DecisionRecord],
) -> Vec<ecaa_workflow_core::lifecycle_adversarial::LifecycleTransition> {
    use ecaa_workflow_core::decision_log::{DecisionActor, DecisionType};
    use ecaa_workflow_core::lifecycle_adversarial::LifecycleTransition;

    let mut out = Vec::new();
    for d in decisions {
        if let DecisionType::AssumptionWaived {
            assumption_id,
            policy_rule_id,
            ..
        } = &d.decision
        {
            if is_blocking_policy_rule(policy_rule_id.as_str()) {
                let actor_label = match d.actor {
                    DecisionActor::Sme => "sme",
                    DecisionActor::Llm => "llm",
                    DecisionActor::Harness => "harness",
                };
                out.push(LifecycleTransition::ForbiddenWaiverAttempt {
                    actor: actor_label.to_string(),
                    assumption_id: assumption_id.clone(),
                    policy_rule_id: policy_rule_id.clone(),
                });
            }
        }
    }
    out
}

pub(crate) fn detect_verifier_unresolvability(
    prior: Option<&ecaa_workflow_core::workflow_contracts::task_node::WorkflowDag>,
    new: &ecaa_workflow_core::workflow_contracts::task_node::WorkflowDag,
) -> Vec<ecaa_workflow_core::lifecycle_adversarial::LifecycleTransition> {
    use ecaa_workflow_core::lifecycle_adversarial::LifecycleTransition;
    use ecaa_workflow_core::workflow_contracts::evidence::{
        AssumptionResolution, AssumptionSource,
    };

    let prior_ids: std::collections::BTreeSet<&str> = prior
        .map(|p| {
            p.assumptions
                .entries
                .iter()
                .map(|e| e.id.as_str())
                .collect()
        })
        .unwrap_or_default();

    let mut out = Vec::new();
    for a in &new.assumptions.entries {
        if prior_ids.contains(a.id.as_str()) {
            continue;
        }
        let is_profiler = matches!(a.source, AssumptionSource::ProfilerDegraded { .. });
        let is_unresolved = matches!(a.resolution, AssumptionResolution::Unresolved);
        if is_profiler && is_unresolved {
            let reason = match &a.source {
                AssumptionSource::ProfilerDegraded { reason, .. } => reason.clone(),
                _ => "profiler discovered the assumption is unresolvable".into(),
            };
            out.push(LifecycleTransition::VerifierUnresolvability {
                assumption_id: a.id.clone(),
                verifier: "data_profiler".to_string(),
                reason,
            });
        }
    }
    out
}

pub(crate) fn detect_production_revocation(
    prior: Option<&ecaa_workflow_core::workflow_contracts::task_node::WorkflowDag>,
    new: &ecaa_workflow_core::workflow_contracts::task_node::WorkflowDag,
) -> Vec<ecaa_workflow_core::lifecycle_adversarial::LifecycleTransition> {
    use ecaa_workflow_core::workflow_contracts::lifecycle::production_revocation_cascade;

    let prior_dag = match prior {
        Some(p) => p,
        None => return Vec::new(),
    };
    let mut out = Vec::new();
    for new_node in &new.nodes {
        if let Some(prior_node) = prior_dag.nodes.iter().find(|p| p.id == new_node.id) {
            if let Some(t) = production_revocation_cascade(
                new_node.id.clone(),
                prior_node.lifecycle_state,
                new_node.lifecycle_state,
                format!(
                    "node demoted from {:?} to {:?}",
                    prior_node.lifecycle_state, new_node.lifecycle_state
                ),
                vec![new.id.clone()],
            ) {
                out.push(t);
            }
        }
    }
    out
}

pub(crate) fn enqueue_adjudication(
    session: &mut Session,
    transition: ecaa_workflow_core::lifecycle_adversarial::LifecycleTransition,
) {
    use ecaa_workflow_core::decision_log::{DecisionActor, DecisionType};
    use ecaa_workflow_core::decision_substrate::{record, stable_id, timestamp, VerifierDecision};
    use ecaa_workflow_core::lifecycle_adversarial::{
        AdjudicationQueueEntry, AdjudicationStatus, LifecycleTransition,
    };

    if session
        .adjudication_queue
        .iter()
        .any(|e| e.transition == transition)
    {
        return;
    }

    let entry_id = format!("adj_{}", lifecycle_uuid_short());
    let payload = serde_json::to_string(&transition).unwrap_or_else(|_| String::from("{}"));
    let transition_kind = transition.kind().to_string();

    // v3 P8 follow-up — emit the detection event into the v4 P3
    // verifier-decision substrate. Paired one-to-one below with the
    // `AdjudicationEnqueued` row so the F18 substrate-completeness
    // property test can assert
    // `queue_writes == substrate_enqueues`.
    record(VerifierDecision::LifecycleAdversarialEdgeDetected {
        id: stable_id("lae", &transition_kind, transition.affected_node_id()),
        timestamp: timestamp(),
        transition_kind: transition_kind.clone(),
        affected_node_id: transition.affected_node_id().to_string(),
        rationale: transition.rationale(),
    });

    match &transition {
        LifecycleTransition::SameUserContradiction {
            assumption_id,
            prior_record_id,
            new_record_id,
            ..
        } => {
            session.record_decision(
                DecisionType::ContradictionDetected {
                    assumption_id: assumption_id.clone(),
                    prior_id: prior_record_id.clone(),
                    new_id: new_record_id.clone(),
                },
                DecisionActor::Harness,
                None,
            );
        }
        LifecycleTransition::CrossUserConflict {
            assumption_id,
            records,
            ..
        } if records.len() >= 2 => {
            session.record_decision(
                DecisionType::ContradictionDetected {
                    assumption_id: assumption_id.clone(),
                    prior_id: records[0].clone(),
                    new_id: records[1].clone(),
                },
                DecisionActor::Harness,
                None,
            );
        }
        LifecycleTransition::UpstreamInvalidation {
            assumption_id,
            affected_downstream,
            ..
        } => {
            session.record_decision(
                DecisionType::InvalidationCascaded {
                    assumption_id: assumption_id.clone(),
                    affected: affected_downstream.clone(),
                },
                DecisionActor::Harness,
                None,
            );
        }
        _ => {}
    }

    session.record_decision(
        DecisionType::LifecycleTransition {
            transition_kind: transition_kind.clone(),
            payload,
        },
        DecisionActor::Harness,
        None,
    );

    session.adjudication_queue.push(AdjudicationQueueEntry {
        id: entry_id.clone(),
        created_at: ecaa_workflow_core::time_helpers::now_rfc3339(),
        transition,
        status: AdjudicationStatus::Open,
    });

    // v3 P8 follow-up — second substrate row paired with the
    // `LifecycleAdversarialEdgeDetected` emission above. Captures the
    // queue write so F18's substrate-completeness property test can
    // assert one substrate row per queue entry.
    let substrate_id = stable_id("aen", &entry_id, &transition_kind);
    record(VerifierDecision::AdjudicationEnqueued {
        id: substrate_id,
        timestamp: timestamp(),
        queue_entry_id: entry_id,
        transition_kind,
    });
}

fn lifecycle_uuid_short() -> String {
    Uuid::new_v4()
        .to_string()
        .chars()
        .filter(|c| c.is_ascii_hexdigit())
        .take(12)
        .collect()
}

fn are_opposite_resolutions(a: &str, b: &str) -> bool {
    let positive = |s: &str| matches!(s, "accepted" | "resolved_by_validator");
    let negative = |s: &str| matches!(s, "rejected" | "contradicted");
    (positive(a) && negative(b)) || (negative(a) && positive(b))
}

fn is_blocking_policy_rule(rule_id: &str) -> bool {
    rule_id.ends_with(":clinical") || rule_id.ends_with(":phi_strict")
}

fn infer_goal_for_modalities(
    archetype_reg: &ecaa_workflow_core::archetype_registry::ArchetypeRegistry,
    project_class: &str,
    modalities: &[&str],
    source_prose: &str,
) -> Option<ecaa_workflow_core::goal_spec::GoalSpec> {
    if let Some(goal) =
        infer_exact_cross_omics_goal(archetype_reg, project_class, modalities, source_prose)
    {
        return Some(goal);
    }

    // Pass 1: per-modality archetype lookup.
    for modality in modalities {
        let branch = archetype_reg
            .iter()
            .map(|(_, archetype)| archetype)
            .filter(|archetype| {
                archetype.project_class == project_class
                    && archetype.cross_omics_modalities.is_empty()
                    && archetype.modality_hint.as_deref() == Some(*modality)
            })
            .min_by(|a, b| a.id.cmp(&b.id));
        if let Some(archetype) = branch {
            return Some(goal_from_archetype(archetype, source_prose));
        }
    }

    // Pass 2: project-class fallback. Project-class-routed
    // archetypes (clinical_trial_analysis, time_series_forecast) leave
    // `modality_hint` unset because the workflow is project-class-driven
    // rather than modality-driven. The classifier always sets
    // `modality = generic_omics` for these inputs (no modality keywords
    // matched), so Pass 1's modality_hint match returns nothing. Pass 2
    // recovers by picking the canonical project-class-routed archetype
    // (smallest atom set among those whose `modality_hint.is_none()`).
    let project_class_match = archetype_reg
        .iter()
        .map(|(_, archetype)| archetype)
        .filter(|archetype| {
            archetype.project_class == project_class
                && archetype.cross_omics_modalities.is_empty()
                && archetype.modality_hint.is_none()
        })
        .min_by(|a, b| {
            a.atoms
                .len()
                .cmp(&b.atoms.len())
                .then_with(|| a.id.cmp(&b.id))
        });
    if let Some(archetype) = project_class_match {
        return Some(goal_from_archetype(archetype, source_prose));
    }

    tracing::warn!(
        requested_modalities = ?modalities,
        project_class,
        "rebuild_dag: unable to infer a composer goal for intake (no archetype matched modality, project_class, or cross_omics_modalities)"
    );
    None
}

fn infer_exact_cross_omics_goal(
    archetype_reg: &ecaa_workflow_core::archetype_registry::ArchetypeRegistry,
    project_class: &str,
    modalities: &[&str],
    source_prose: &str,
) -> Option<ecaa_workflow_core::goal_spec::GoalSpec> {
    if modalities.len() < 2 {
        return None;
    }
    let want: std::collections::BTreeSet<&str> = modalities.iter().copied().collect();
    let exact_cross = archetype_reg
        .iter()
        .map(|(_, archetype)| archetype)
        .filter(|archetype| {
            archetype.project_class == project_class && !archetype.cross_omics_modalities.is_empty()
        })
        .filter(|archetype| {
            let have: std::collections::BTreeSet<&str> = archetype
                .cross_omics_modalities
                .iter()
                .map(String::as_str)
                .collect();
            have == want
        })
        .min_by(|a, b| a.id.cmp(&b.id));
    if let Some(archetype) = exact_cross {
        return Some(goal_from_archetype(archetype, source_prose));
    }
    None
}

fn goal_from_archetype(
    archetype: &ecaa_workflow_core::archetype::ArchetypeDefinition,
    source_prose: &str,
) -> ecaa_workflow_core::goal_spec::GoalSpec {
    let mut modifiers = std::collections::BTreeMap::new();
    if let Some(kind) = &archetype.goal_kind_hint {
        modifiers.insert("kind".to_string(), kind.clone());
    }
    ecaa_workflow_core::goal_spec::GoalSpec {
        edam_data: archetype.goal_data.clone(),
        edam_format: archetype.goal_format.clone(),
        modifiers,
        source_prose: Some(format!(
            "inferred from archetype metadata: {} ({})",
            archetype.id,
            source_prose.trim()
        )),
        confidence: 0.5,
    }
}

/// Validate that `discover_<stage>` is a task in the current
/// DAG. Dedupes the check shared by `set_intake_method`. Returns an
/// `Err(ToolError::unknown_stage)` with the DAG's discovery stage ids
/// as alternatives when the lookup fails.
pub(super) fn validate_discover_stage(session: &Session, stage: &str) -> Result<(), ToolError> {
    let discover_id = format!("discover_{}", stage);
    let stage_known = session
        .dag
        .as_ref()
        .map(|d| d.tasks.contains_key(discover_id.as_str()))
        .unwrap_or(false);
    if stage_known {
        return Ok(());
    }
    let alternatives: Vec<String> = session
        .dag
        .as_ref()
        .map(|d| {
            d.tasks
                .keys()
                .filter_map(|k| k.as_str().strip_prefix("discover_").map(String::from))
                .collect()
        })
        .unwrap_or_default();
    Err(ToolError::unknown_stage(
        stage,
        alternatives,
        "Strip any execute-task prefix (e.g. use 'annotation' not 'cell_type_annotation') — set_intake_method pins the corresponding discover_<stage> task.",
    ))
}

/// Invalidate the forward slice of `stage` in the session's
/// DAG and rebuild. Dedupes the `amend_stage_method` +
/// `select_sensitivity_winner` pair; both tools need the same
/// "downstream tasks regenerate from the mutated method" effect.
/// Returns the list of invalidated task ids on success.
pub(super) fn invalidate_and_rebuild(
    session: &mut Session,
    stage: &str,
    config_dir: &std::path::Path,
) -> Result<Vec<String>, ToolError> {
    // Phase D refactor: read the forward slice from the cache (lowered
    // workflow_dag), then clear those tasks' runtime states so the
    // re-derivation after rebuild_dag shows them as Pending. The cache
    // itself is invalidated explicitly and re-derived by the next read.
    let invalidated: Vec<String> = session
        .current_dag()
        .map(|mut d| {
            d.invalidate_forward_slice(stage, true)
                .into_iter()
                .map(|id| id.to_string())
                .collect()
        })
        .unwrap_or_default();
    for id in &invalidated {
        session.task_states.remove(id);
    }
    #[allow(deprecated)] // deliberate cache-reset for non-workflow_dag state change
    session.invalidate_dag();
    rebuild_dag(session, config_dir)?;
    Ok(invalidated)
}

pub(super) fn state_delta(session: &Session, stage: &str) -> serde_json::Value {
    let dag = session.current_dag();
    let dag = dag.as_ref();
    let (completed, ready, blocked, pending) = dag.map(|d| d.progress()).unwrap_or((0, 0, 0, 0));
    serde_json::json!({
        "stage": stage,
        "task_count": dag.map(|d| d.tasks.len()).unwrap_or(0),
        "completed": completed,
        "ready": ready,
        "blocked": blocked,
        "pending": pending,
        "state": session.state,
    })
}
