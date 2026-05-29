//! session data types.
//!
//! Pure data (no state-machine logic, no blocker fallback logic, no
//! branch-lineage logic). The state machine lives in `transitions.rs`;
//! the blocker fallback lives in `blocker_shim.rs`; the lineage helper
//! lives in `lineage.rs`; decision-log helpers live in
//! `decision_helpers.rs`. `impl Session { pub fn new }` stays in
//! `mod.rs` as the primary constructor.

use super::{IntakeMethodsSerde, SessionId, SessionLineage};
use chrono::{DateTime, Utc};
use std::collections::BTreeMap;

/// Session-scoped registry of renderer proposals recorded by
/// `propose_hypothesized_renderer`. Keyed by `proposal_id`; ordered by
/// `BTreeMap` so serialized JSON is deterministic across runs.
///
/// Persisted with the session via `#[serde(default)]` so existing on-disk
/// sessions without this field deserialize with an empty registry.
#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
pub struct RendererProposals {
    proposals: BTreeMap<String, RendererProposal>,
}

impl RendererProposals {
    /// Append a new proposal. Caller must have already de-duped via
    /// `find_duplicate`; this method does NOT check for duplicates.
    pub fn push_proposal(
        &mut self,
        id: String,
        target_semantic_type: String,
        proposed_parent_terms: Vec<String>,
        proposed_figure_ids: Vec<String>,
        sme_intent: String,
        primitive_basis: Option<String>,
    ) {
        self.proposals.insert(
            id.clone(),
            RendererProposal {
                id,
                target_semantic_type,
                proposed_parent_terms,
                proposed_figure_ids,
                sme_intent,
                primitive_basis,
                created_at: chrono::Utc::now(),
            },
        );
    }

    /// Return the `proposal_id` of an existing proposal for the same
    /// `(target_semantic_type, sorted_figure_ids)` pair, or `None` if
    /// no such proposal has been recorded. Used for idempotency in the
    /// tool handler.
    pub fn find_duplicate(
        &self,
        target_semantic_type: &str,
        sorted_figure_ids: &[String],
    ) -> Option<String> {
        for (id, p) in &self.proposals {
            let mut existing_sorted = p.proposed_figure_ids.clone();
            existing_sorted.sort();
            if p.target_semantic_type == target_semantic_type
                && existing_sorted == sorted_figure_ids
            {
                return Some(id.clone());
            }
        }
        None
    }

    /// Number of proposals currently recorded.
    pub fn len(&self) -> usize {
        self.proposals.len()
    }

    /// True when no proposals have been recorded yet.
    pub fn is_empty(&self) -> bool {
        self.proposals.is_empty()
    }
}

/// A single renderer proposal recorded by `propose_hypothesized_renderer`.
/// The proposal is `Hypothesized` / `Unverified` until catalog-promotion
/// evidence accumulates. Never mutates the `PlotAffordanceRegistry` directly.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct RendererProposal {
    /// Stable id assigned at proposal time (e.g. `renderer-proposal-<12hex>`).
    pub id: String,
    /// The output-port semantic type IRI the renderer addresses
    /// (e.g. `ecaax:my_custom_output`).
    pub target_semantic_type: String,
    /// Registered parent-term IRIs the proposed renderer inherits from
    /// (validated against the `PlotAffordanceRegistry` at proposal time).
    pub proposed_parent_terms: Vec<String>,
    /// Figure ids the preferred renderer would produce (none may shadow a
    /// registered figure id at proposal time).
    pub proposed_figure_ids: Vec<String>,
    /// LLM-summarized SME description, ≤ 800 chars.
    pub sme_intent: String,
    /// The structural primitive id the SME is upgrading from, if any
    /// (e.g. `__structural_matrix_overview`). `None` when not based on a
    /// known fallback.
    pub primitive_basis: Option<String>,
    /// UTC timestamp when the proposal was recorded.
    pub created_at: DateTime<Utc>,
}

/// `#[serde(default = "default_schema_version")]` callback. Returns the
/// canonical session SemVer so sessions persisted before the field
/// existed load unchanged.
///
/// Promoted from `u32` to `semver::Version`. Backward-compat
/// reads are handled by `crate::migration::schema_version_serde` on
/// the `Session::schema_version` field.
fn default_schema_version() -> semver::Version {
    ecaa_workflow_core::migration::current_session_version()
}

/// Server-side audit of which stages the SME has
/// explicitly named a method for. Gates `set_intake_method`: the tool
/// refuses to execute unless `named.get(&stage_id).copied().unwrap_or(false)`
/// is true, blocking the LLM from auto-pinning methods on its own.
///
/// The UI sets the flag through
/// `POST /api/chat/session/:id/intake-method/:stage_id/sme-named` when
/// the SME clicks a quick-reply method chip or types a method name
/// unprompted in a structured intake form. The flag is keyed by stage id
/// and stays `true` once set; amendments (which delegate to the agent's
/// best-practice scorer, not the SME's named choice) implicitly reset
/// the gate by routing through `amend_stage_method`, not `set_intake_method`.
///
/// `#[serde(default)]` so existing on-disk sessions without this field
/// deserialize cleanly with an empty signals map.
#[derive(Clone, Debug, Default, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(default)]
pub struct SmeMethodSignals {
    /// Per-stage flag: `true` once the SME explicitly named or
    /// accepted a method for that stage id. Stored as `BTreeMap` so
    /// serialized JSON is deterministic across runs.
    pub named: BTreeMap<String, bool>,
}

/// `#[serde(default = "default_composer_version")]` callback. Returns 1
/// (legacy taxonomy build) for sessions persisted before this field
/// existed; new sessions get the current composer default at construction.
fn default_composer_version() -> u32 {
    1
}

/// Custom deserializer that accepts both the legacy
/// `user_confirmed: bool` shape and the new
/// `confirmation_token: Option<ConfirmationToken>` shape on the same
/// wire field. Sessions persisted before the ConfirmationToken
/// migration carry the legacy bool; the adapter folds them through to
/// `None` (fail-safe re-confirm requirement) so an SME loading a
/// pre-migration session is forced to re-click Confirm before
/// `emit_package` succeeds. Sessions persisted after this migration
/// carry the new optional-token shape and round-trip unchanged.
fn deserialize_confirmation_token_legacy<'de, D>(
    de: D,
) -> Result<Option<crate::session::ConfirmationToken>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Shape {
        // New shape: an Option<ConfirmationToken> directly on the
        // field. `None` here = no SME confirmation. `Some(t)` = SME
        // confirmed this specific (emission_id, summary_hash).
        New(Option<crate::session::ConfirmationToken>),
        // Legacy shape: a bare `bool`. On disk this looked like
        // `"user_confirmed": true|false`.
        LegacyBool(bool),
    }

    match Shape::deserialize(de)? {
        Shape::New(v) => Ok(v),
        Shape::LegacyBool(true) => {
            // Persisted session from before the C2 migration carried
            // user_confirmed=true. We can't fabricate a token
            // retroactively (no `emission_id` + `summary_hash` to bind
            // to), so the safe answer is `None`: the SME re-confirms
            // on next interaction. This is the explicit fail-safe per
            // the remediation plan; the alternative — silently
            // promoting `true` to a sentinel token — would mean
            // emit_package could fire on a stale plan whose summary
            // hash never matched anything the SME actually saw.
            tracing::warn!(
                "legacy user_confirmed=true session loaded; clearing latch \
                 (C2 migration; SME will re-confirm)"
            );
            Ok(None)
        }
        Shape::LegacyBool(false) => Ok(None),
    }
}

use ecaa_workflow_core::blocker::{BlockerContext, BlockerEntry, BlockerKind};
use ecaa_workflow_core::classify::ClassificationResult;
use ecaa_workflow_core::dag::DAG;
use ecaa_workflow_core::decision_log::DecisionRecord;
use ecaa_workflow_core::hypothesized_proposal::{HypothesizedProposal, ProposalId};
use ecaa_workflow_core::lifecycle_adversarial::AdjudicationQueueEntry;
use ecaa_workflow_core::taxonomy::StageTaxonomy;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;
use ts_rs::TS;
use uuid::Uuid;

/// Live chat session. Holds all intake state, conversation history, and
/// execution context for one SME interaction. Persisted to disk as JSON
/// on every mutation via `SessionStore`. The state machine lives in
/// `transitions.rs`; construction is in `mod.rs`.
#[derive(Debug, Clone, Serialize, Deserialize, TS, schemars::JsonSchema)]
#[ts(export)]
pub struct Session {
    /// Migration rail for the session schema. v1 = legacy taxonomy-driven
    /// build; v2 = archetype path; v3 = backward-chain composer.
    ///
    /// Stored as `semver::Version`. The
    /// `crate::migration::schema_version_serde` adapter accepts both
    /// legacy bare-`u64` values and canonical SemVer strings on read,
    /// so on-disk sessions persisted under the old `u32` shape
    /// deserialize unchanged. Writes always emit the canonical SemVer
    /// string. `#[serde(default = "default_schema_version")]` returns
    /// the current SemVer for sessions persisted before this field
    /// existed; new sessions get the current default via
    /// `Session::new`.
    #[serde(
        default = "default_schema_version",
        with = "ecaa_workflow_core::migration::schema_version_serde"
    )]
    #[ts(type = "string")]
    #[schemars(with = "String")]
    pub schema_version: semver::Version,
    /// Which composer this session committed to. Distinct from
    /// `schema_version` because a session's data shape and its
    /// composer choice can evolve independently. v1 = legacy
    /// taxonomy-driven build (always the value today); v2 = archetype
    /// fast-path; v3 = backward-chain composer. Pinned at session
    /// creation; amendments stay on the same composer so re-emission
    /// is byte-deterministic. `#[serde(default = "default_composer_version")]`
    /// returns 1 for sessions persisted before this field existed.
    #[serde(default = "default_composer_version")]
    pub composer_version: u32,
    /// Pilot sizing actuation. Set by the server's
    /// `POST /api/chat/session/:id/progress` handler when the
    /// harness reports a `sizing_pilot_complete` event with a full
    /// `PilotReport` payload. Read by the AwsExecutor /
    /// SlurmExecutor at provisioning time so the full-run
    /// instance-type / sbatch shape uses the pilot's projection
    /// rather than the conservative default.
    ///
    /// Stored as a JSON Value rather than a typed `PilotReport`
    /// because the type lives in `crates/harness::executor::pilot`
    /// (which `crates/conversation` doesn't depend on by design).
    /// The executor reads it back via `serde_json::from_value` at
    /// the dispatch site.
    ///
    /// Default `None` for sessions persisted before this field
    /// existed; set once per session lifecycle (post-pilot, before
    /// the first non-pilot task runs).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(type = "unknown | null")]
    pub pilot_recommendation: Option<serde_json::Value>,
    /// Stable session identifier (UUID v4).
    #[ts(type = "string")]
    pub id: SessionId,
    /// UTC timestamp when the session was first created.
    #[ts(type = "string")]
    pub created_at: DateTime<Utc>,
    /// UTC timestamp of the most recent turn or mutation.
    #[ts(type = "string")]
    pub last_activity: DateTime<Utc>,
    /// Current position in the session state machine.
    pub state: SessionState,
    /// `Arc<Vec<Turn>>`. Tool-loop iterations clone the Arc
    /// (pointer bump) instead of the full Vec. Mutations use
    /// `Arc::make_mut(&mut session.conversation).push(...)` which
    /// copies-on-write only when there's more than one holder — and
    /// the tool loop's shared holder drops before the next mutation
    /// anyway, so the typical mutation is still in-place.
    #[ts(as = "Vec<Turn>")]
    pub conversation: Arc<Vec<Turn>>,
    /// Accumulated free-form text describing the analysis the SME wants to run.
    pub intake_prose: String,
    /// Per-stage method overrides recorded by `set_intake_method`.
    #[serde(default)]
    #[ts(skip)]
    pub intake_methods: IntakeMethodsSerde,
    /// SME-declared atom exclusions for sub-archetype small-task
    /// scenarios. Populated by `set_intake_excluded_atoms` when the
    /// SME provides post-processed input (count matrix, BAM, processed
    /// object) or explicitly opts out of a pipeline stage. Empty by
    /// default. On rebuild the post-build pass prunes every task whose
    /// id is in this set, along with its `discover_<id>` /
    /// `validate_<id>` companions and any orphaned downstream task
    /// whose only surviving inputs were the excluded ones; downstream
    /// atoms outside this set that lose all upstreams get rewired to
    /// `data_acquisition` instead of cascade-dropped.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[ts(skip)]
    pub excluded_atoms: Vec<String>,
    /// Server-side audit gate for `set_intake_method`.
    /// The tool refuses to fire unless the SME has explicitly named a
    /// method for the target stage via a UI affordance (quick-reply chip
    /// or structured intake form). Prevents the LLM from auto-pinning
    /// method choices the SME never approved.
    #[serde(default)]
    #[ts(skip)]
    pub sme_method_signals: SmeMethodSignals,
    /// Classifier output produced by `classify_intake` / `append_intake_prose`.
    #[serde(default)]
    #[ts(skip)]
    pub classification: Option<ClassificationResult>,
    /// Loaded stage taxonomy for the classified modality.
    #[serde(default)]
    #[ts(skip)]
    pub taxonomy: Option<StageTaxonomy>,
    /// Unified
    /// WorkflowIntent. Strict superset of `classification.goal` +
    /// `intake_facts` carrying goal, available_data, desired
    /// outputs, constraints, uncertainties, privacy/regulatory
    /// flags, execution preferences, and explanation preferences.
    /// Materialized by the intake tool (`classify_intake`,
    /// `append_intake_prose`) rather than by the LLM directly.
    /// Populated alongside `classification` rather than replacing
    /// it; sessions persisted before this field existed load with
    /// `None`.
    #[serde(default)]
    #[ts(skip)]
    pub workflow_intent:
        Option<ecaa_workflow_core::workflow_contracts::workflow_intent::WorkflowIntent>,
    /// Populated once the classifier has run. Defaults to
    /// `Bioinformatics` so sessions persisted before this field
    /// existed load unchanged. The LLM reads this via
    /// `get_session_state`; it does not author it.
    #[serde(default)]
    pub project_class: ecaa_workflow_core::project_class::ProjectClass,
    /// Session-level confirmatory/exploratory discipline. Locked at
    /// first `POST /confirm`. Defaults to `Exploratory` so sessions
    /// persisted before this field existed load unchanged.
    #[serde(default)]
    pub mode: ecaa_workflow_core::session_mode::SessionMode,
    /// True once `POST /confirm` has fired, which locks `mode` for the
    /// remainder of the session.
    #[serde(default)]
    pub mode_locked: bool,
    /// Project-level checkpoint discipline. Defaults to `Gated` so
    /// sessions persisted before this field existed load unchanged.
    #[serde(default)]
    pub checkpoint_mode: ecaa_workflow_core::checkpoint_mode::CheckpointMode,
    /// Memoization cache for the v4 `workflow_dag` lowered to legacy
    /// `DAG`. Authority is `workflow_dag` + `task_states`. Readers MUST
    /// use [`Session::current_dag`] / [`Session::ensure_dag_cached`];
    /// callers that rebuild workflow_dag MUST invalidate via
    /// [`Session::invalidate_dag`]. See `derived_dag.rs` for rationale.
    #[serde(default)]
    #[ts(skip)]
    pub dag: Option<DAG>,
    /// Authoritative per-task runtime state cache. Keyed by task id.
    /// The harness writes Pending → Running → Completed / Failed /
    /// Blocked here; `current_dag()` overlays onto the lowered DAG.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    #[ts(skip)]
    pub task_states: std::collections::BTreeMap<String, ecaa_workflow_core::dag::TaskState>,
    /// Per-emit confirmation latch. `None` means the SME has NOT
    /// approved the current plan shape (or never has). `Some(token)`
    /// binds the SME's `/confirm` click to a specific
    /// `(emission_id, summary_hash)` pair; `emit_package` verifies the
    /// token authorizes the current pending emission AND that the
    /// plan summary hasn't drifted since the SME clicked.
    ///
    /// Backward-compat: the custom serde adapter
    /// [`deserialize_confirmation_token_legacy`] accepts the legacy
    /// `user_confirmed: bool` field shape and folds it through to
    /// `None` (fail-safe re-confirm requirement) so sessions
    /// persisted before this migration force one re-confirm round
    /// on next interaction. The `#[serde(alias = "user_confirmed")]`
    /// lets serde read the legacy key transparently on disk; new
    /// writes go to the canonical `confirmation_token` key.
    ///
    /// Wire-shape stability: external tooling that read the legacy
    /// `user_confirmed: bool` should switch to reading
    /// `SessionStateSnapshot::user_confirmed` (the server-side wire
    /// projection in `chat_routes/sessions.rs`), which still has the
    /// boolean type and the legacy field name. The session
    /// persistence file's internal shape is not a contracted external
    /// API.
    #[serde(
        default,
        alias = "user_confirmed",
        deserialize_with = "deserialize_confirmation_token_legacy"
    )]
    pub confirmation_token: Option<crate::session::ConfirmationToken>,
    /// The pending emission this session is about to perform. Populated
    /// when transitioning into `PendingConfirmation`; cleared when the
    /// state machine leaves that state (Confirm → ReadyToEmit retains
    /// it through emit, Reject / amend clears it). The confirmation
    /// token's `emission_id` must equal this exact UUID for the
    /// `emit_package` precondition to pass.
    ///
    /// `#[serde(default)]` so sessions persisted before this field
    /// existed deserialize cleanly with `None` (the token deser legacy
    /// fallback handles the matching `user_confirmed: bool` field).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(type = "string | null")]
    pub pending_emission_id: Option<Uuid>,
    /// Filesystem path of the most recently emitted RO-Crate package.
    #[serde(default)]
    #[ts(type = "string | null")]
    pub emitted_package_path: Option<PathBuf>,
    /// Streaming progress events posted by the harness (batch-flushed into a synthetic assistant turn).
    #[serde(default)]
    pub harness_events: Vec<HarnessEvent>,
    /// Rolling audit log of tool calls made during this session.
    #[serde(default)]
    pub tool_call_log: Vec<ToolCallRecord>,
    /// Append-only SME decision audit trail. One record per high-leverage
    /// checkpoint (confirm, reject, unblock, branch, emit, amend, rerun,
    /// sensitivity winner). Serialized into `runtime/decisions.jsonl` at
    /// emit time and registered as an RO-Crate `CreativeWork`.
    #[serde(default)]
    pub decisions: Vec<DecisionRecord>,
    /// When true, routes every turn to the Opus 4.7 escalation tier regardless of state or confidence.
    #[serde(default)]
    pub careful_mode: bool,
    /// Tracks whether the current Blocked-state episode has already
    /// consumed its one-shot Opus escalation. Resets to false every
    /// time the session transitions INTO Blocked; set to true after
    /// `ModelPolicy::choose_with_reason` picks Opus for the Blocked
    /// trigger. Prevents every turn during a long SME decision wait
    /// from paying Opus rates while the SME stares at the card.
    #[serde(default)]
    #[ts(skip)]
    pub blocked_opus_escalation_consumed: bool,
    /// When this session was
    /// branched from a parent, this carries the parent's session id +
    /// the timestamp the branch happened + the conversation turn index
    /// the branch was taken from. None for root sessions. Persisted
    /// across reloads so the SessionTree endpoint can rebuild the
    /// lineage graph at any time.
    #[serde(default)]
    pub lineage: Option<SessionLineage>,
    /// Operator-visible short name for this session. Populated lazily
    /// via `POST /api/chat/session/:id/auto-title`, which runs a
    /// single Haiku 4.5 call over the first few turns through
    /// `ModelPolicy::for_side_call()`. Persists across reloads;
    /// idempotent once set (the route short-circuits when
    /// `title.is_some()` rather than re-invoking the LLM). `None` for
    /// sessions persisted before this field existed via
    /// `#[serde(default)]` so they load unchanged; the UI falls back
    /// to a shortened session id when rendering the session tree.
    #[serde(default)]
    #[ts(optional)]
    pub title: Option<String>,
    /// Session-level soft budget cap in USD. SME-authored via
    /// `POST /api/chat/session/:id/budget`; None means no cap.
    /// Persists across reloads.
    #[serde(default)]
    #[ts(optional)]
    pub budget_usd: Option<f64>,
    /// Who set the budget — SME username from the session envelope, or
    /// "env-default" when populated from `ECAA_DEFAULT_SESSION_BUDGET_USD`.
    #[serde(default)]
    #[ts(optional)]
    pub budget_set_by: Option<String>,
    /// When the budget was last set / changed.
    #[serde(default)]
    #[ts(optional, type = "string")]
    pub budget_set_at: Option<DateTime<Utc>>,
    /// Read-only share tokens. A request carrying
    /// `?share_token=<token>` or `X-Share-Token: <token>` is gated
    /// into read-only mode by the middleware in
    /// `crates/server/src/read_only.rs`. Empty for sessions that have
    /// never been shared.
    #[serde(default)]
    #[ts(skip)]
    pub share_tokens: Vec<ShareToken>,
    /// SME-supplied data inputs registered before / during intake.
    /// When non-empty, the data_acquisition stage's discovery layer
    /// prefers `sme_supplied_local_path` / `sme_supplied_uploaded_files`
    /// over public-repository fetchers. See `crates/server/src/chat_routes/inputs.rs`
    /// for the registration endpoints and `config/downstream-policy/best-practice-scoring-policy.json`
    /// for the candidate-method definitions. Defaults to empty so
    /// sessions persisted before this field existed load unchanged.
    #[serde(default)]
    pub inputs: Vec<UserInput>,
    /// Filesystem paths the path-hint extractor pulled out of SME
    /// intake prose that resolve under `ECAA_INPUT_ROOTS` but the SME
    /// hasn't formally registered yet. The LLM sees these via
    /// `get_session_state` and the UI renders them as a "Detected
    /// inputs — register?" affordance. Auto-registered + cleared
    /// during `append_intake_prose` when `ECAA_AUTO_REGISTER_PROSE_PATHS`
    /// is enabled. Empty for sessions persisted before this field
    /// existed (`#[serde(default)]`).
    #[serde(default)]
    pub pending_input_hints: Vec<crate::intake_path_hints::InputPathHint>,
    /// Owner of this session. Today (single-tenant) this defaults to
    /// "local". Once a fronting auth proxy is in place the
    /// `X-Scripps-User` (or `X-Forwarded-User`) header populates this
    /// at session-create time. Listing endpoints will filter by this
    /// in Phase F. Stored, not yet enforced — that's a separate
    /// permission layer (read-only mode for non-owners).
    #[serde(default = "default_owner_user")]
    pub owner_user: String,
    /// Carries `(target_stage, prior_emit_path,
    /// rationale)` from `amend_stage_method` (Emitted → Amending →
    /// ReadyToEmit) into the next `emit_package` call so the conversation
    /// emit wrapper can populate `EmitConfig::amend_from` +
    /// `EmitConfig::amend_context`. Set by `amend_stage_method` and
    /// `select_sensitivity_winner` (the two paths that route through
    /// `AmendStart`); cleared by the emit wrapper after a successful
    /// emit so re-emitting the same package on a fresh ReadyToEmit
    /// (no amend) doesn't fabricate amendment lineage.
    ///
    /// `#[serde(default, skip_serializing_if = "Option::is_none")]` so
    /// sessions persisted before this field existed deserialize as
    /// `None`. ts-rs `skip` because the field is internal — the UI
    /// reads `SessionState::Amending` for surface display, not this.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(skip)]
    pub pending_amendment: Option<PendingAmendment>,
    /// Typed v4 planner output cache.
    ///
    /// Populated by `tools::rebuild_dag` on every successful v4
    /// composition (composer_version == 4). The chat_routes/compose
    /// endpoints serve `compose-outcome`, `compose-alternatives`,
    /// and `policy-decisions` directly from this cache so the UI
    /// can render the typed Composition tab without re-running the
    /// planner. Persisted across session reloads via serde so the
    /// Composition tab stays populated when a session is restored
    /// from disk; the on-disk `runtime/proofs.jsonl` /
    /// `runtime/assumptions.jsonl` sidecars are the secondary
    /// source consulted by the route handlers when this cache
    /// is `None` (legacy sessions or pre-v4 emit history).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(skip)]
    pub workflow_dag: Option<ecaa_workflow_core::workflow_contracts::task_node::WorkflowDag>,
    /// Cached v4 typed compose outcome
    /// (`ValidatedExecutableDag` / `DraftDag` / `PartialDag` /
    /// `NovelNodeSpec` / `Refusal`). See `workflow_dag` above for
    /// persistence rationale.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(skip)]
    pub compose_outcome: Option<ecaa_workflow_core::workflow_contracts::outcome::ComposeOutcome>,
    /// Top-K ranked alternatives from the v4 planner.
    /// Empty for v1/v2/v3 sessions and v4 sessions with only one
    /// composition produced.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[ts(skip)]
    pub ranked_alternatives: Vec<ecaa_workflow_core::composer_v4::RankedAlternative>,
    /// Recorded per-node policy decisions from the v4
    /// planner's policy gate. Empty for non-v4 sessions and v4
    /// sessions with no active policy bundle.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[ts(skip)]
    pub policy_decisions: Vec<ecaa_workflow_core::composer::PolicyDecisionRecord>,
    /// Active policy bundle id for this session.
    /// `None` = no policy enforcement; v4 composition runs the
    /// per-node policy gate against the matching bundle when set.
    /// Activated by `POST /api/chat/session/:id/policy-bundle`
    /// (typically wired through `ClinicalConfirmGate` for clinical-
    /// trial sessions). Persisted across reloads so re-emission
    /// preserves the policy stance.
    ///
    /// Recognized values:
    /// - `clinical_trial` → `PolicyContext::clinical_trial_bundle()`
    /// - `phi_strict` → `PolicyContext::phi_strict_bundle()`
    ///
    /// Unknown values resolve to no-op (logged as a warning).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub active_policy_bundle: Option<String>,
    /// Transient post-handler state-machine triggers
    /// the dispatcher's `post_handler.on_ok` hook drains and fires.
    /// Mutating tools push state transitions here rather than calling
    /// `try_transition` directly inside their handler bodies. Always
    /// empty across persistence boundaries (`#[serde(skip)]`); always
    /// drained by the dispatcher before returning. Append-only inside
    /// a handler; iteration order is the firing order, so handlers
    /// that need a chain (e.g. `amend_stage_method` firing
    /// `AmendStart` followed by `AmendReady`) push in sequence.
    #[serde(skip)]
    #[ts(skip)]
    pub deferred_state_triggers: Vec<crate::session::transitions::StateTrigger>,
    /// Cargo.lock-style pin of the archetype this session committed
    /// to at first emit. Snapshotted from the `ArchetypeRegistry` at
    /// the moment the composer's archetype fast-path matched.
    /// Subsequent re-emissions (on amendment) re-compose against THIS
    /// snapshot, not the live registry, so an archetype semver bump
    /// (e.g. additive slot added in `single_cell_de v1.1.0`) doesn't
    /// retroactively rebase a session that was already running on
    /// `v1.0.0`.
    ///
    /// `None` for sessions whose composer never matched an
    /// archetype (composer_version = 1 legacy taxonomy build) OR
    /// for sessions persisted before this field existed. The
    /// composer reads it via `Session::archetype_snapshot` if Some,
    /// else falls through to live registry lookup.
    ///
    /// ts-rs `skip` because the snapshot is large (the entire
    /// archetype YAML) and the UI doesn't need to render it
    /// directly — it gets `archetype_id` + `archetype_version`
    /// summary from `composer_decision` records instead.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(skip)]
    pub archetype_snapshot: Option<ecaa_workflow_core::archetype::ArchetypeDefinition>,
    /// Flexible plotting upgrade plan session-scoped registry of
    /// renderer proposals created by `propose_hypothesized_renderer`. Keyed
    /// by `proposal_id`. Persisted via `#[serde(default)]` so existing
    /// on-disk sessions without this field deserialize with an empty registry.
    ///
    /// Proposals are `Hypothesized` / `Unverified` until catalog-promotion
    /// evidence accumulates (validator → sandbox → SME-signoff promotion
    /// pipeline). The tool handler is the only write path; the affordance
    /// resolver and the UI Result Review card are the read paths.
    #[serde(default)]
    #[ts(skip)]
    pub renderer_proposals: RendererProposals,
    /// Session-scoped
    /// registry of hypothesized **node** proposals created by
    /// `propose_hypothesized_node`. Keyed by `ProposalId`; `BTreeMap`
    /// (not `HashMap`) so serialized JSON is deterministic.
    ///
    /// Distinct from `renderer_proposals` (which targets the plot
    /// affordance registry). Each proposal walks a three-gate
    /// promotion pipeline (validator → sandbox → SME signoff);
    /// the runner lives at [`crate::proposal_gate`]. The tool handler
    /// writes; the server endpoints + UI card read.
    ///
    /// `#[serde(default, skip_serializing_if = "BTreeMap::is_empty")]`
    /// so existing on-disk sessions without this field deserialize
    /// cleanly with an empty map, and sessions that never used the
    /// new flow keep their JSON payload identical.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    #[ts(skip)]
    pub proposals: BTreeMap<ProposalId, HypothesizedProposal>,
    /// Flexible plotting upgrade plan session-scoped counter of
    /// structural-fallback events recorded by the affordance resolver when
    /// it cannot find a catalog entry for a `(semantic_type, primitive)` pair.
    ///
    /// Skipped in serialization (`#[serde(skip)]`) because the counter is
    /// a transient in-memory aggregator populated during the current server
    /// uptime; the durable form is the `runtime/affordance_fallbacks.jsonl`
    /// sidecar written at emit time. Sessions loaded from disk after a
    /// restart start with an empty counter — the metrics endpoint will show
    /// an empty list until new fallback events are recorded. This mirrors
    /// the `deferred_state_triggers` pattern: always-empty across
    /// persistence boundaries.
    ///
    /// Write path: affordance resolver calls `record` when it falls back.
    /// Read path: `metrics_snapshot` drains `all_gaps_sorted_by_count_desc`
    /// into `SessionMetrics::affordance_fallbacks`.
    #[serde(skip)]
    #[ts(skip)]
    pub affordance_fallback_counter:
        ecaa_workflow_core::plot_affordance::AffordanceFallbackCounter,
    /// V3 session-scoped adjudication queue for the six
    /// non-monotonic lifecycle edges from design §7. Populated by
    /// `tools::rebuild_dag` when a non-monotonic lifecycle edge fires.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub adjudication_queue: Vec<AdjudicationQueueEntry>,
    /// Atom-safety-policy session-scoped widening of an
    /// atom's `runtime_packages` set. Keyed by `atom_id` → `registry`
    /// → set of package strings. Populated by the
    /// `POST /api/chat/session/:id/atom/:atom_id/add-runtime-package`
    /// endpoint when the SME clicks the BlockerCard's
    /// `ProvisioningDenied` "Add `<package>` to atom.runtime_packages"
    /// affordance. The harness install-proxy reads the merged set
    /// (declared catalog + this override) when checking provisioning
    /// authority on retry.
    ///
    /// `BTreeMap` rather than `HashMap` so JSON serialization is
    /// byte-deterministic across emits; `BTreeSet` per registry so
    /// duplicate adds (idempotent click-through) don't grow the set.
    /// `#[serde(default, skip_serializing_if = "BTreeMap::is_empty")]`
    /// keeps the JSON payload byte-identical for sessions that never
    /// touched this field — pre-A.S6 sessions on disk deserialize
    /// cleanly with an empty map.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    #[ts(skip)]
    pub atom_runtime_overrides: std::collections::BTreeMap<
        String,
        std::collections::BTreeMap<String, std::collections::BTreeSet<String>>,
    >,
    /// Count of
    /// consecutive turns the session has ended in `IntakeFollowup`.
    /// Bumped by `Session::note_turn_end_intake_followup` (called by
    /// `tool_loop::run_tool_loop` exactly once per successful exit path);
    /// reset to 0 whenever the turn ends in any other state. Read by
    /// `build_system_prompt` to surface a convergence nudge at `>= 4`
    /// so the LLM stops looping clarifying questions.
    ///
    /// Per-turn (not per-transition): an in-turn `AppendProse` from
    /// `IntakeFollowup` lands in `Intake` and the subsequent rebuild
    /// fires `DagBuiltWithUnresolvedDiscovery` to re-enter
    /// `IntakeFollowup`. A per-transition increment would reset inside
    /// every turn and never accumulate across turns.
    ///
    /// `#[serde(default)]` keeps existing on-disk sessions loading
    /// cleanly (they re-zero on load, then re-accumulate as the session
    /// continues). `#[ts(skip)]` because the field is internal — the UI
    /// surfaces conversation length / followup-turn count via metrics,
    /// not via this counter.
    #[serde(default)]
    #[ts(skip)]
    pub intake_followup_streak: u32,

    /// The `run_id` (UUID v4 string) of the most recently emitted package.
    /// Set by the conversation emit wrapper after a successful
    /// `emit_with_conversation_log` call. Exposed via `get_session_state`
    /// so the UI's Performance tab + harness progress handler can correlate
    /// events to the package that generated them. `None` until the first
    /// successful emission. `#[serde(default)]` so sessions persisted
    /// before this field existed load cleanly with `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub last_emitted_run_id: Option<String>,

    /// Per-session 32-byte secret for HMAC-SHA256 audit sidecars (C5).
    ///
    /// Written as a lowercase 64-char hex string on disk so it survives
    /// JSON round-trips without loss. Generated with `OsRng` at session
    /// creation; never rotated within a session (rows written before an
    /// emit remain verifiable for the lifetime of the session).
    ///
    /// Migration: sessions persisted before this field existed load with
    /// `[0u8; 32]` (the default below) and then have a fresh secret
    /// re-generated and saved on the next `SessionStore::update` cycle.
    /// The zero-secret case is detected by `audit_writer_secret_is_zero`
    /// in `persistence.rs` and triggers the migration write.
    ///
    /// `#[ts(skip)]` — the UI never sees the secret; it lives only in
    /// the session persistence file (server-side, never transmitted to
    /// the browser).
    #[serde(
        default = "default_audit_writer_secret",
        with = "audit_secret_hex_serde"
    )]
    #[ts(skip)]
    #[schemars(with = "String")]
    pub audit_writer_secret: [u8; 32],

    /// Outstanding disambiguation pair the SME hasn't answered.
    /// When set, `propose_quick_replies` surfaces the pair's prompt
    /// instead of the LLM's natural follow-up question. Cleared
    /// when the SME selects a quick reply via
    /// `clear_disambiguation_on_selection`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub pending_disambiguation: Option<String>,
}

/// Default factory used by `#[serde(default)]` when loading a session
/// persisted before the `audit_writer_secret` field existed. Returns
/// the zero array; `persistence.rs` detects this sentinel and
/// regenerates + saves a real secret on the next update.
fn default_audit_writer_secret() -> [u8; 32] {
    [0u8; 32]
}

/// Hex-string serde adapter for `[u8; 32]`. Serializes as a 64-char
/// lowercase hex string; deserializes from the same format. Stored in
/// the session JSON next to other `#[ts(skip)]` internals.
mod audit_secret_hex_serde {
    use serde::{Deserialize, Deserializer, Serializer};

    pub(super) fn serialize<S: Serializer>(secret: &[u8; 32], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&hex::encode(secret))
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<[u8; 32], D::Error> {
        let hex_str = String::deserialize(d)?;
        let bytes = hex::decode(&hex_str).map_err(serde::de::Error::custom)?;
        if bytes.len() != 32 {
            return Err(serde::de::Error::custom(format!(
                "audit_writer_secret must be 32 bytes (64 hex chars), got {} bytes",
                bytes.len()
            )));
        }
        let mut arr = [0u8; 32];
        arr.copy_from_slice(&bytes);
        Ok(arr)
    }
}

/// Amendment context carried from `AmendStart` to the
/// next `emit_package` so RO-Crate lineage (`prov:wasDerivedFrom`,
/// `UpdateAction`) and `policies/amendment-lineage.json` populate
/// correctly. Modeled here (not in `core::emitter::AmendContext`) so
/// the conversation crate can persist it across the
/// AmendStart → AmendReady → EmitPackageStart hops without depending
/// on crate-private state.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS, schemars::JsonSchema)]
#[ts(export)]
pub struct PendingAmendment {
    /// The stage whose method is being replaced.
    pub target_stage: String,
    /// Downstream task ids invalidated by the method swap (transitive closure).
    pub invalidated_tasks: Vec<String>,
    /// Parent package path captured at `AmendStart`-time from
    /// `session.emitted_package_path`. The next `emit_package` call
    /// will overwrite that field with the child path; we capture the
    /// parent here so `EmitConfig::amend_from` can point at the parent
    /// crate.
    #[ts(type = "string")]
    pub parent_package_path: PathBuf,
    /// Optional SME-supplied rationale for the method swap. Threaded
    /// into `AmendContext::reason` and surfaced on the `UpdateAction`
    /// entity's `description` field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub rationale: Option<String>,
}

fn default_owner_user() -> String {
    // Resolve at deserialize time so legacy session files coming back
    // from disk get a stable label rather than blank-string. The same
    // resolver runs at create time in `Session::new`.
    std::env::var("USER").unwrap_or_else(|_| "local".to_string())
}

/// SME-registered data input. Two flavors today:
///
/// - `local_path` — SME pointed at a directory already on the server
///   filesystem (validated against `ECAA_INPUT_ROOTS` allowlist).
/// - `uploaded_files` — SME uploaded files through the UI; they live
///   under `<ECAA_UPLOAD_ROOT>/<session_id>/`.
///
/// In both cases the server walks the tree at registration time,
/// computes per-file size + sha256, and stores a manifest here. The
/// agent reads the manifest from CONTEXT.md (rendered at emit time)
/// and from a sibling `runtime/inputs.json` so downstream stages can
/// consume the canonical paths without re-walking.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS, schemars::JsonSchema)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub struct UserInput {
    /// Stable per-session id (uuid4 hex, 16 chars). Used by the UI
    /// "Remove" button and by the `set_intake_data_source` tool to
    /// reference a specific registration.
    pub input_id: String,
    /// SME-friendly label. Falls back to the directory basename when
    /// not supplied at registration.
    pub label: String,
    /// Whether this input arrived as a local filesystem path or via file upload.
    pub kind: UserInputKind,
    /// Absolute, canonicalized path. For `local_path` this is the
    /// SME-registered directory. For `uploaded_files` this is the
    /// per-session upload subdir.
    pub root_path: String,
    /// Per-file inventory built at registration time.
    pub files: Vec<UserInputFile>,
    /// UTC timestamp when the input was registered.
    #[ts(type = "string")]
    pub registered_at: DateTime<Utc>,
    /// SME identity at registration time (matches the session's
    /// `owner_user` today; could diverge later if collaborators share
    /// a session).
    pub registered_by: String,
}

/// How the SME supplied their input data to the session.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, TS, schemars::JsonSchema)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum UserInputKind {
    /// SME pointed the system at an already-on-disk directory.
    LocalPath,
    /// SME uploaded files through the UI.
    UploadedFiles,
}

/// One file within a `UserInput` registration.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS, schemars::JsonSchema)]
#[ts(export)]
pub struct UserInputFile {
    /// Path relative to `UserInput.root_path`. The agent joins
    /// `root_path / relpath` to read.
    pub relpath: String,
    /// File size in bytes at registration time.
    pub size_bytes: u64,
    /// Hex sha256. Computed at registration time. Verified again on
    /// access by the agent if it needs cache invariance.
    pub sha256: String,
}

/// A read-only access token for this session.
///
/// The persisted field is the SHA-256
/// hex digest of the bearer token, NOT the plaintext. The plaintext
/// is returned to the issuer exactly once at `POST /share-token` time
/// and is never again recoverable from the session JSON. The middleware
/// hashes any presented token and uses constant-time compare against
/// `token_hash`.
///
/// `serde(alias = "token")` accepts legacy session files that had the
/// plaintext token stored as `token` — those values load into
/// `token_hash` but won't match any SHA-256 hex digest in the compare,
/// so they're effectively invalidated until the operator re-issues.
/// This is the documented migration behavior; release notes flag it.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ShareToken {
    /// SHA-256 hex digest of the original 32-byte plaintext token.
    /// 64 hex chars. Compare via `subtle::ConstantTimeEq` to defeat
    /// timing-side-channel exfiltration.
    #[serde(alias = "token")]
    pub token_hash: String,
    /// UTC timestamp when the token expires. `None` is treated as
    /// already-expired by the middleware — legacy never-expires
    /// entries fail closed under the mandatory-TTL discipline.
    pub expires_at: Option<DateTime<Utc>>,
    /// UTC timestamp when the share token was issued.
    pub created_at: DateTime<Utc>,
}

/// State-machine node for a chat session. Drives the LLM dispatch
/// loop, tool precondition checks, and UI recovery affordances.
/// The `transitions.rs` module owns the full transition table;
/// the `#[non_exhaustive]` attribute preserves forward compatibility
/// with downstream consumers.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS, schemars::JsonSchema)]
#[ts(export)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum SessionState {
    /// Initial state — waiting for the SME's first message.
    Greeting,
    /// Actively gathering / refining intake; classifier has run.
    Intake,
    /// Intake is partially specified; DAG was built but has unresolved discovery stages.
    IntakeFollowup,
    /// SME confirmation gate. `stage: None` is the existing
    /// emission confirmation (propose_summary_confirmation → Confirm).
    /// `stage: Some(id)` is a per-stage review gate introduced by the
    /// `requires_sme_review: true` taxonomy flag — dispatch pauses
    /// until `POST /confirm { stage: "<id>" }` lands. Absent on
    /// sessions persisted before this field existed; `#[serde(default)]`
    /// defaults to `None`, reproducing the earlier unit-variant shape on
    /// the wire (`{"kind":"pending_confirmation"}`).
    PendingConfirmation {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        /// Stage id awaiting confirmation, if the confirmation is stage-scoped.
        stage: Option<String>,
    },
    /// Intake complete and SME has confirmed; `emit_package` is now allowed.
    ReadyToEmit,
    /// The `emit_package` handler is in flight; the package is being written to disk.
    Emitting,
    /// Package has been successfully emitted; the harness may be running.
    Emitted,
    /// Post-emission, the SME
    /// asked to swap a method for a previously-emitted stage. The
    /// session is rebuilding its DAG slice before re-emitting a
    /// lineage-linked amendment package. The conversation-visible
    /// effect is that emit_package is gated until the transition lands
    /// in ReadyToEmit again.
    Amending {
        /// The stage whose method the SME is swapping.
        target_stage: String,
        /// Downstream task ids that were invalidated (transitive closure)
        /// — surfaced so the UI Jobs tab can explain which tasks are
        /// about to rerun.
        invalidated_tasks: Vec<String>,
    },
    /// Session is blocked; the SME must take a recovery action via the
    /// `BlockerCard` UI before the harness can continue.
    Blocked {
        /// Queue of active blockers. New sessions populate this
        /// alongside the legacy fields below so concurrent task-level
        /// blockers are not overwritten by the latest one.
        #[serde(default)]
        blockers: Vec<BlockerEntry>,
        /// Legacy human-readable reason — kept as the canonical wire field
        /// for serde forward-/backward-compat with sessions persisted
        /// before the typed migration. Populated alongside `blocker_kind`
        /// for every new transition; callers prefer `blocker_kind` for
        /// structured dispatch.
        reason: String,
        /// Legacy recovery hint. Same migration rationale as `reason`.
        recovery_hint: String,
        /// Typed blocker taxonomy. Absent on sessions persisted before
        /// the typed migration — callers fall back to synthesizing
        /// `BlockerKind::AgentError { message: reason }` when None. The
        /// 30-day session TTL auto-expires the None branch.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        blocker_kind: Option<BlockerKind>,
        /// Structured context (timestamp + recovery hints) alongside the
        /// typed kind.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        context: Option<BlockerContext>,
    },
}

/// One conversation turn (user, assistant, or system message).
#[derive(Debug, Clone, Serialize, Deserialize, TS, schemars::JsonSchema)]
#[ts(export)]
pub struct Turn {
    /// Stable identifier for this turn (UUID v4).
    #[ts(type = "string")]
    pub turn_id: Uuid,
    /// Who authored this turn.
    pub role: TurnRole,
    /// Text content of the turn.
    pub content: String,
    /// Coarse intent classification for assistant turns (used by the UI to pick a rendering style).
    #[serde(default)]
    pub intent: Option<AssistantIntent>,
    /// Tool calls the assistant made during this turn.
    #[serde(default)]
    pub tool_calls: Vec<ToolCallRecord>,
    /// Quick-reply chips the assistant suggested at the end of this turn.
    #[serde(default)]
    pub quick_replies: Vec<String>,
    /// Confirmation card rendered for this turn, if the assistant called `propose_summary_confirmation`.
    #[serde(default)]
    pub confirmation_card: Option<ConfirmationCard>,
    /// UTC timestamp when this turn was created.
    #[ts(type = "string")]
    pub timestamp: DateTime<Utc>,
}

/// Authorship role of a conversation turn.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS, schemars::JsonSchema)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum TurnRole {
    /// Message authored by the SME (user).
    User,
    /// Message authored by the LLM on behalf of the assistant.
    Assistant,
    /// Injected system message (not shown in the SME-facing chat pane).
    System,
}

/// Coarse intent classification attached to assistant turns. Used by the
/// UI to choose the appropriate rendering style (e.g. `SummaryConfirm` → show the
/// confirm card; `Blocker` → show the blocker card).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS, schemars::JsonSchema)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum AssistantIntent {
    /// Opening turn of a new session.
    Greeting,
    /// Acknowledgement of SME input with no structural change.
    Acknowledge,
    /// Asking the SME to clarify an ambiguous aspect of the intake.
    Clarify,
    /// Presenting the plan summary card for SME confirmation.
    SummaryConfirm,
    /// Sharing an intermediate recommendation or question.
    Recommend,
    /// Follow-up turn after a successful emission.
    PostEmission,
    /// Surfacing a blocker that requires SME action.
    Blocker,
}

/// Audit record for a single tool call made during a turn.
#[derive(Debug, Clone, Serialize, Deserialize, TS, schemars::JsonSchema)]
#[ts(export)]
pub struct ToolCallRecord {
    /// Turn this tool call belongs to.
    #[ts(type = "string")]
    pub turn_id: Uuid,
    /// Canonical snake_case name of the tool (matches `Tool::name()`).
    pub tool_name: String,
    /// Arguments the LLM passed to the tool.
    #[ts(type = "unknown")]
    pub args: serde_json::Value,
    /// Serialized `ToolResult` returned by the dispatcher.
    #[ts(type = "unknown")]
    pub result: serde_json::Value,
    /// True when the dispatcher returned a `ToolError`.
    pub is_error: bool,
    /// Anthropic model id that produced this tool call.
    pub model: String,
    /// UTC timestamp when the tool was dispatched.
    #[ts(type = "string")]
    pub timestamp: DateTime<Utc>,
}

/// Plan-summary card rendered by `propose_summary_confirmation`. Persisted
/// on the turn so the UI can re-render after page reload.
#[derive(Debug, Clone, Serialize, Deserialize, TS, schemars::JsonSchema)]
#[ts(export)]
pub struct ConfirmationCard {
    /// Markdown summary of the current plan presented to the SME for approval.
    pub summary_markdown: String,
    /// SHA-256 hex digest of `summary_markdown`
    /// at the moment the card was raised. The UI renders the leading
    /// 12 hex chars as a "summary fingerprint" so the SME can verify
    /// the durable audit record references the exact text they saw.
    /// The `/confirm` endpoint attaches the same hash to the
    /// `DecisionType::Confirm` audit record, so a later replayer can
    /// match the recorded fingerprint to the text in `intake-conversation.jsonl`
    /// and detect tampering / drift between the displayed card and the
    /// confirmed text.
    ///
    /// `#[serde(default)]` so existing on-disk sessions whose cards
    /// pre-date this field deserialize cleanly with an empty hash —
    /// the legacy records carry no fingerprint, which is fine because
    /// they also pre-date the matching audit-record field.
    #[serde(default)]
    pub summary_hash: String,
    /// Coarse resource estimate aggregated from each
    /// atom's `resource_profile`. Rendered inline so the SME sees a
    /// "≈ N core-hours, peak M GB" cost preview before clicking
    /// Accept. None for legacy / non-composer paths.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub resource_estimate: Option<ecaa_workflow_core::composer::ResourceEstimate>,
}

/// Progress event posted by the harness to the server's `/progress` endpoint.
/// Batched by `harness_batch.rs` into synthetic assistant turns.
#[derive(Debug, Clone, Serialize, Deserialize, TS, schemars::JsonSchema)]
#[ts(export)]
pub struct HarnessEvent {
    /// Event kind string (e.g. `"task_started"`, `"task_completed"`, `"task_blocked"`).
    pub kind: String,
    /// Task id the event refers to (matches a `WORKFLOW.json` task id).
    pub task_id: String,
    /// Human-readable status label (e.g. `"Running"`, `"Completed"`).
    pub status: String,
    /// Free-text detail message for this event.
    pub detail: String,
    /// Remote-execution metadata when the harness is running against a
    /// cloud backend. None for local execution. Additive-serde
    /// (`#[serde(default, skip_serializing_if =...)]`) so existing
    /// on-disk session JSON and existing ProgressClient payloads without
    /// the field continue to deserialize.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub remote: Option<RemoteExecutionInfo>,
    /// UTC timestamp when the event was posted.
    #[ts(type = "string")]
    pub timestamp: DateTime<Utc>,
}

/// Subset of `crates/core::dag::RemoteExecution` the harness reports on
/// progress events. Only the three fields the UI actually renders —
/// backend badge, instance id for the jobs feed, instance type for the
/// sizing chip — cross the session boundary. `command_id` and
/// `output_uri` stay in the DAG.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS, schemars::JsonSchema)]
#[ts(export)]
pub struct RemoteExecutionInfo {
    /// Backend label (e.g. `"aws"`, `"slurm"`).
    pub backend: String,
    /// Cloud/HPC instance identifier for the jobs feed.
    pub instance_id: String,
    /// EC2 or SLURM partition instance type string.
    pub instance_type: String,
}

/// Inline structured-capture card the LLM can render when dense input
/// would make the freeform composer awkward. The AWS sizing layer
/// uses this to prompt for
/// sample_count/coverage_depth/cell_count/database_size_gb; captured
/// values flow into `IntakeFacts`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS, schemars::JsonSchema)]
#[ts(export)]
pub struct StructuredCaptureTurnCard {
    /// Card heading shown to the SME.
    pub title: String,
    /// Optional explanatory subtitle shown below the card heading.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub description: Option<String>,
    /// Ordered list of fields the SME must fill in.
    pub fields: Vec<StructuredCaptureField>,
    /// Preseeded values the card should render as defaults. Optional.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    #[ts(type = "Record<string, string>")]
    pub initial_values: std::collections::BTreeMap<String, String>,
}

/// One field inside a `StructuredCaptureTurnCard`. Matches the shape the
/// existing UI component `StructuredCaptureTurnCard.tsx` already consumes.
/// The `kind` field lets the UI pick an input type (integer vs. float
/// vs. plain string) and lets the harness reject inputs that violate
/// the declared type without a round-trip. The four AWS-sizing
/// canonical keys — `sample_count`, `coverage_depth`, `cell_count`,
/// `database_size_gb` — are documented here but not enumerated in the
/// Rust type (tool calls requesting them use these exact keys; the
/// sizing layer reads them back out of the captured values).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS, schemars::JsonSchema)]
#[ts(export)]
pub struct StructuredCaptureField {
    /// Stable key the conversation service uses to round-trip the
    /// value. For AWS-sizing fields use one of the canonical names:
    /// sample_count, coverage_depth, cell_count, database_size_gb.
    pub key: String,
    /// Human-readable field label shown above the input widget.
    pub label: String,
    /// Optional placeholder text shown inside the input widget when empty.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub placeholder: Option<String>,
    /// If true, the SME must supply a non-empty value before the card can be submitted.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub required: bool,
    /// If true, render as a textarea instead of a single-line input.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub multiline: bool,
    /// Input-type hint. The UI renders everything as a text input
    /// (the generic component doesn't type-switch) but the
    /// conversation service validates captured values against this
    /// kind before handing them to `IntakeFacts`.
    #[serde(default)]
    pub kind: StructuredCaptureFieldKind,
}

/// Type hint for a `StructuredCaptureField`. The UI renders all fields
/// as text inputs; the conversation service uses this to validate
/// captured values before forwarding them to `IntakeFacts`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, TS, schemars::JsonSchema)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum StructuredCaptureFieldKind {
    /// Arbitrary UTF-8 text (default).
    #[default]
    String,
    /// Whole-number value; validated with `i64::from_str`.
    Integer,
    /// Floating-point value; validated with `f64::from_str`.
    Float,
}

impl Turn {
    /// Construct a new user turn with the given content.
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            turn_id: Uuid::new_v4(),
            role: TurnRole::User,
            content: content.into(),
            intent: None,
            tool_calls: vec![],
            quick_replies: vec![],
            confirmation_card: None,
            timestamp: Utc::now(),
        }
    }

    /// Construct a new assistant turn with the given content.
    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            turn_id: Uuid::new_v4(),
            role: TurnRole::Assistant,
            content: content.into(),
            intent: None,
            tool_calls: vec![],
            quick_replies: vec![],
            confirmation_card: None,
            timestamp: Utc::now(),
        }
    }
}
