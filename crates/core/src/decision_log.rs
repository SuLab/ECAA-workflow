//! SME decision audit trail for the full-lifecycle chat.
//!
//! Every high-leverage checkpoint — confirm / reject / unblock / branch /
//! emit / amend / rerun / sensitivity winner — produces one
//! [`DecisionRecord`]. Records are append-only; the emitter writes them to
//! `runtime/decisions.jsonl` inside the package and registers the file as an
//! RO-Crate `CreativeWork` so the audit trail ships with the emitted output.
//!
//! This is the structured analog of the raw `ToolCallRecord` log. Tool calls
//! capture *every* LLM dispatch; a decision captures only the SME- or
//! LLM-driven mutations that change the analysis contract. The two coexist:
//! the tool log is the full transcript, the decision log is the summary.
//!
//! Lives in `core` (not `conversation`) so the types can be `#[derive(TS)]`-
//! exported into `ui/src/types/` without pulling the conversation crate into
//! the dependency arrow, matching the pattern set by `blocker.rs`.

use crate::ids::{AtomId, StageId, TaskId};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::workflow_contracts::chain_of_custody::ChainOfCustody;
use crate::workflow_contracts::policy_rule_id::PolicyRuleId;

/// Current on-disk schema version for [`DecisionRecord`].
pub fn current_decision_record_version() -> semver::Version {
    crate::migration::current_decision_record_schema_version()
}

fn default_decision_record_schema_version() -> semver::Version {
    current_decision_record_version()
}

/// Who originated the decision.
///
/// - `Sme`: a REST-endpoint button click (`/confirm`, `/reject`, `/unblock`,
///   `/branch`) where the SME is the proximate actor.
/// - `Llm`: a tool-dispatch decision (e.g. the LLM fired `emit_package`
///   after the SME confirmed). The alone-in-turn gate means these are
///   always authorised by a prior SME action, but the LLM is the proximate
///   caller.
/// - `Harness`: the harness / orchestrator recorded the decision on the
///   session (e.g. resume-from-blocker). Not currently emitted but
///   reserved so the enum is forward-compatible.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, TS, schemars::JsonSchema)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum DecisionActor {
    /// Sme variant.
    Sme,
    /// Llm variant.
    Llm,
    /// Harness variant.
    Harness,
}

/// Closed taxonomy of decisions worth auditing. Internally-tagged so the
/// Serialized JSON is flat (`{"kind":"amend_stage",...}`). Adding a new
/// variant requires a plan amendment — the decision log is part of the
/// reproducibility contract.
///
/// `EnumCount` is derived so test fixtures that enumerate variants stay
/// in sync with the enum at compile time (F42).
#[derive(
    Debug, Clone, Serialize, Deserialize, PartialEq, TS, strum::EnumCount, schemars::JsonSchema,
)]
#[ts(export)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DecisionType {
    /// The SME clicked **Confirm** on the summary card. Gates
    /// `emit_package`.
    ///
    /// When the confirmation card carried a `summary_hash` (SHA-256
    /// of the displayed markdown), the same fingerprint is attached
    /// here so an audit replayer can match the recorded confirmation
    /// to the exact text the SME saw. None for legacy records whose
    /// cards pre-date the hashing logic and for confirm calls that
    /// happened outside the `propose_summary_confirmation` flow
    /// (e.g. test fixtures, scripted offline confirms).
    Confirm {
        /// Hex SHA-256 of the `summary_markdown` the SME saw on the
        /// confirmation card. `#[serde(default)]` keeps the legacy
        /// unit-variant on-disk form (`{"kind":"confirm"}`)
        /// deserializable: it loads with `summary_hash = None`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        summary_hash: Option<String>,
    },
    /// The SME clicked **Make corrections**.
    Reject,
    /// The SME cleared a blocker from the `BlockerCard`.
    Unblock,
    /// A new branched session was forked from this one. The child's id
    /// is recorded so the two logs can be cross-referenced.
    Branch { child_session_id: String },
    /// The compiler emitted a package to disk. `output_dir` is the
    /// absolute path of the emitted package root.
    EmitPackage { output_dir: String },
    /// A stage's method was swapped post-emission.
    AmendStage { stage: String, method_prose: String },
    /// A completed task was re-queued with its existing method.
    /// `reason` carries the free-form justification passed through
    /// the `rerun_task` tool.
    RerunTask {
        /// Task id.
        task_id: TaskId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        /// Reason.
        reason: Option<String>,
    },
    /// The user picked a winner from a sensitivity comparison. `stage`
    /// identifies the comparison; `winner` names the chosen variant.
    ///
    /// The candidates the SME *rejected* are captured here at
    /// decision-time. Without this field, the
    /// `Blocked { AwaitingSmeSelection { candidates } }` list is
    /// dropped on transition and the audit trail loses the
    /// counterfactual — replay can't tell whether the SME picked scVI
    /// over `[CCA]` (one-of-two) or `[CCA, Harmony, fastMNN]`
    /// (one-of-four). Both sides of the choice are durable.
    /// `#[serde(default)]` keeps legacy on-disk records (which omit
    /// the field entirely) loading as an empty Vec.
    SelectSensitivityWinner {
        /// Stage.
        stage: String,
        /// Winner.
        winner: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        /// Rejected candidates.
        rejected_candidates: Vec<String>,
    },
    /// The harness emitted a cross-version concordance report after an
    /// amend or branch re-emission. Both package paths are recorded so
    /// readers (UI History tab, lotz ledger drift report, future
    /// regulatory exporter) can resolve the diff files without
    /// re-running `cross_version_diff`.
    CrossVersionDiff {
        /// Parent package.
        parent_package: String,
        /// Child package.
        child_package: String,
        /// Overall concordance.
        overall_concordance: f64,
        /// N discordant.
        n_discordant: usize,
    },
    /// Fired when a post-emission `amend_stage_method` or `rerun_task`
    /// targets a stage that was declared prespecified in
    /// `SessionMode::Confirmatory`. A non-empty rationale is required
    /// by the tool handler; `reason` carries it here for the audit trail.
    PostHocDeviation {
        /// Target stage.
        target_stage: String,
        /// Prior method.
        prior_method: String,
        /// New method.
        new_method: String,
        /// Reason.
        reason: String,
    },
    /// Fired when `CheckpointMode::{Fast,Selective}` skips an SME
    /// review gate that Gated mode would have paused on. The audit log
    /// captures the stage + which mode did the skip so post-run review
    /// can always reconstruct whether a stage was explicitly approved
    /// or auto-advanced.
    AutoAdvanced { stage: String, mode: String },
    /// The SME answered a structured-blocker decision point via the
    /// BlockerCard picker (or the equivalent REST endpoint). Written
    /// once per decision_point per answer round; a blocker with N
    /// decision_points produces N records. Surfaced in the Decisions
    /// tab so the audit trail is complete end-to-end.
    AppliedStructuredDecision {
        /// Task id.
        task_id: TaskId,
        /// Decision point id.
        decision_point_id: String,
        /// Chosen.
        chosen: String,
    },
    /// Fired when the server ingests a new `sme_disposition.json` file
    /// (either via an agent `disposition_proposed` progress event or a
    /// backfill scan). Captures the disposition's origin task + author
    /// timestamp + how many actions it contains so the Decisions tab
    /// surfaces the proposal independently of its eventual
    /// acceptance / rejection.
    DispositionProposed {
        /// Path of the disposition file relative to the emitted
        /// package root, e.g. `runtime/outputs/results_review/sme_disposition.json`.
        path: String,
        /// `task_id` field from inside the disposition — not the same
        /// as the disposition's file-system task directory when the
        /// agent cross-writes (rare but permitted).
        task_id: TaskId,
        /// RFC-3339 from the disposition's `created_at` when present;
        /// server fills with its own now() when the field is absent.
        created_at: String,
        /// Action count.
        action_count: usize,
    },
    /// Fired once per action during `/dispositions/:path/apply` (or
    /// the per-action variant). `outcome` is `"ok"` or `"err"`;
    /// `error_reason` is populated only on the latter. `auto` flags the
    /// double-gate escape hatch — `false` on SME-clicked applies.
    DispositionApplied {
        /// Path.
        path: String,
        /// Action index.
        action_index: usize,
        /// Action kind.
        action_kind: String,
        /// Target stage.
        target_stage: String,
        /// Outcome.
        outcome: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        /// Error reason.
        error_reason: Option<String>,
        /// Auto.
        auto: bool,
    },
    /// Fired when the SME rejects a disposition via
    /// `/dispositions/:path/reject`. `rationale` is optional but
    /// strongly encouraged — the SME's justification for declining the
    /// agent's plan is valuable audit context. Not mutation-bearing:
    /// no action runs.
    DispositionRejected {
        /// Path.
        path: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        /// Rationale.
        rationale: Option<String>,
    },
    /// Fired when the SME reverses a just-applied amendment via the
    /// Undo toast before the harness has re-run the invalidated
    /// forward slice. `stage` identifies the stage whose prose
    /// round-trips; `reverted_to` is the prior method prose the
    /// session now carries.
    UndoneAmendment { stage: String, reverted_to: String },
    /// Recorded when the SME sets or clears the session-level soft
    /// budget cap. `prior_usd` is the ceiling the change replaced
    /// (None when no cap was set); `new_usd` is the new ceiling (None
    /// when the SME cleared the cap).
    BudgetChanged {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        /// Prior usd.
        prior_usd: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        /// New usd.
        new_usd: Option<f64>,
    },
    /// The SME attached a freeform note to a task via the Notes
    /// drawer. `body` is the note body verbatim; `author` carries the
    /// session envelope's author when the client passed it (empty
    /// string otherwise). Surfaces in the Decisions tab so multi-person
    /// handoff has a searchable trail.
    UserNote {
        /// Task id.
        task_id: TaskId,
        /// Body.
        body: String,
        #[serde(default)]
        /// Author.
        author: String,
    },
    /// LLM-side `set_intake_field` recorded a structured value for a
    /// stage during intake. `value` is the JSON-encoded payload so
    /// replay can reconstruct `Session.intake_methods` exactly.
    SetIntakeField {
        /// Stage.
        stage: String,
        /// Field.
        field: String,
        #[ts(type = "unknown")]
        /// Value.
        value: serde_json::Value,
    },
    /// LLM-side `set_intake_method` recorded the SME's natural-language
    /// method choice for a stage. Distinct from `AmendStage`, which
    /// covers the post-emission swap path.
    SetIntakeMethod { stage: String, method_prose: String },
    /// LLM-side `append_intake_prose` appended a fragment to the
    /// running intake description. Captures the appended fragment plus
    /// the modality classification so replay can reconstruct the
    /// classifier's path.
    AppendIntakeProse {
        /// Fragment.
        fragment: String,
        /// Classified modality.
        classified_modality: String,
        /// Modality changed.
        modality_changed: bool,
    },
    /// An assumption was recorded during composition (LLM inference,
    /// lossy adapter insertion, missing metadata, profiler
    /// degradation). The decision-log entry is the persistence path
    /// for `AssumptionLedger`; the typed `Assumption` shape lives in
    /// `workflow_contracts::evidence`.
    AssumptionRecorded {
        /// Stable assumption id (`a_<n>` per session).
        id: String,
        /// Free-text statement.
        statement: String,
        /// Source kind (`sme_accepted`, `llm_inferred`,
        /// `lossy_adapter`, `profiler_degraded`,
        /// `policy_exception`, `ontology_mapping_unresolved`).
        source: String,
        /// TaskNode ids the assumption affects.
        affects_nodes: Vec<String>,
        /// Risk class label
        /// (`negligible|low|moderate|high|clinical`).
        risk: String,
    },
    /// An assumption was resolved (accepted, rejected, or validated
    /// by a downstream check).
    AssumptionResolved {
        /// References the previously recorded assumption.
        id: String,
        /// Resolution kind (`accepted`, `rejected`,
        /// `resolved_by_validator`).
        resolution: String,
    },
    /// The LLM proposed a hypothesized node (a new TaskNode the
    /// registry doesn't yet know about). The node enters the
    /// proposals registry with `LifecycleState::Hypothesized` /
    /// `TrustLevel::Unverified` and cannot execute until promotion
    /// evidence accumulates (validators + sandbox + promotion gate).
    /// Rejected proposals (LLM-recoverable schema errors) do *not*
    /// write to the decision log; only accepted proposals are
    /// durable.
    ///
    /// The `llm_rationale` field was removed from the canonical
    /// decision record because it carried free-form LLM narrative
    /// that could not be safely attributed to the SME. The narrative
    /// can still appear in the conversational turn body
    /// for the SME to read; the durable audit trail now keeps only
    /// the structural fields (id + parent terms). Existing on-disk
    /// `decisions.jsonl` records that still carry the field will load
    /// via `#[serde(default, skip_serializing_if = "Option::is_none")]`
    /// without breaking deserialization — the field is preserved as an
    /// optional read-only sidecar for legacy replay but never written.
    ProposedHypothesizedNode {
        /// Stable id assigned by the proposals registry.
        node_id: String,
        /// Proposed parent ontology term IRIs (resolvability
        /// validated against EDAM / local-extension namespace at
        /// proposal time).
        parent_terms: Vec<String>,
        /// Legacy field. New records do not set this; older on-disk
        /// records may still carry it. Deserialize-only — present so
        /// legacy replay doesn't fail, never populated by current
        /// code.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        llm_rationale: Option<String>,
    },
    /// Flexible plotting upgrade plan a `PlotAffordance`
    /// variant was successfully resolved for an output port and the
    /// resolution is durable (written to
    /// `runtime/plot_affordances.jsonl`). `provisional` mirrors
    /// `PlotAffordance::is_provisional()`; every non-`Registered`
    /// variant is provisional. `snapshot_id` is the registry
    /// snapshot the resolution consulted, enabling replay without
    /// re-resolving against a potentially-newer registry.
    ///
    /// Written by the backend emitter when
    /// `EmitContext::emit_affordances == Some(_)`. The decision-log
    /// entry duplicates the compact fields from the sidecar so the
    /// Decisions tab can surface affordance provenance without
    /// loading the full JSONL sidecar.
    PlotAffordanceResolved {
        /// Task id.
        task_id: TaskId,
        /// Port name.
        port_name: String,
        /// Affordance variant.
        affordance_variant: String,
        /// Figure ids.
        figure_ids: Vec<String>,
        /// Provisional.
        provisional: bool,
        /// Snapshot id.
        snapshot_id: String,
    },
    /// Flexible plotting upgrade plan the affordance
    /// resolution pipeline fell back to a `StructuralFallback` or
    /// `Deferred` variant because no registered or inherited renderer
    /// matched the output port's semantic type. Distinct from
    /// `PlotAffordanceResolved` so the Decisions tab can highlight
    /// fallbacks for SME review without scanning the full sidecar.
    ///
    /// `primitive` is the `GenericPrimitive` snake_case tag (for
    /// `StructuralFallback`) or `"deferred"` (for `Deferred`).
    /// `semantic_type` is the port's source semantic-type IRI.
    /// `fallback_reason` is the human-readable text from the
    /// `AffordanceProof.rationale` field, preserved here because the
    /// Decisions-tab viewer doesn't load the full JSONL sidecar.
    PlotAffordanceFallback {
        /// Task id.
        task_id: TaskId,
        /// Port name.
        port_name: String,
        /// Primitive.
        primitive: String,
        /// Semantic type.
        semantic_type: String,
        /// Fallback reason.
        fallback_reason: String,
    },
    /// Flexible plotting upgrade plan the LLM called
    /// `propose_hypothesized_renderer` and the proposal was accepted
    /// (parent-term resolvability + figure-id shadowing checks passed).
    ///
    /// The proposal enters the session-scoped `RendererProposals` registry
    /// with `LifecycleState::Hypothesized` / `TrustLevel::Unverified` and
    /// will not replace the structural fallback until catalog-promotion
    /// evidence accumulates (validators + sandbox + promotion gate).
    /// Rejected proposals do *not* write to the
    /// decision log; only accepted proposals are durable.
    ProposedHypothesizedRenderer {
        /// Stable id assigned by the proposals registry
        /// (e.g. `renderer-proposal-<12hex>`).
        proposal_id: String,
        /// The output-port semantic type IRI the renderer addresses.
        target_semantic_type: String,
        /// LLM-summarized SME description, ≤ 800 chars.
        sme_intent: String,
    },
    /// Flexible plotting upgrade plan the renderer drafter
    /// side-call was dispatched for a proposal. Fired once per draft
    /// invocation even if the drafter returns an error; the model
    /// field records which LLM was used (always
    /// `ModelPolicy::for_remediation_proposer()` today).
    RendererDraftRequested {
        /// Proposal id from the session-scoped `RendererProposals`
        /// registry.
        proposal_id: String,
        /// LLM model id used for the draft (e.g. `"opus_4_7"`).
        model: String,
    },
    /// Flexible plotting upgrade plan the renderer drafter
    /// side-call returned a `DraftedRenderer`. Fired on every successful
    /// parse; `lints` preserves the advisory messages from the draft.
    RendererDraftReceived {
        /// Proposal id.
        proposal_id: String,
        /// Advisory lint messages from the drafter (may be empty).
        lints: Vec<String>,
    },
    /// Flexible plotting upgrade plan the static sandbox check
    /// (`check_drafted_renderer`) completed. `outcome` is
    /// `"static_checks_passed"` or `"refused:<count>_refusals"` so the
    /// Decisions tab can show the gate result without loading the full
    /// sandbox report.
    RendererSandboxOutcome {
        /// Proposal id.
        proposal_id: String,
        /// Short outcome tag: `"static_checks_passed"` or
        /// `"refused"`.
        outcome: String,
    },
    /// Flexible plotting upgrade plan the SME approved the
    /// drafted renderer for promotion to the shared affordance catalog.
    /// This decision id is stored as
    /// `RendererPromotionRequest::sme_approval_decision_id` and
    /// referenced in the promoted YAML's provenance header.
    ApproveGeneratedRenderer {
        /// Proposal id.
        proposal_id: String,
        /// SME identity at approval time (from `DecisionActor::Sme`
        /// context, typically the session's `owner_user`).
        approver: String,
    },
    /// Flexible plotting upgrade plan the SME rejected the
    /// drafted renderer. The proposal stays in the session registry but
    /// cannot be promoted without a fresh draft + new approval cycle.
    RejectGeneratedRenderer {
        /// Proposal id.
        proposal_id: String,
        /// Free-form reason for the rejection.
        reason: String,
    },
    /// Flexible plotting upgrade plan `promote_renderer`
    /// completed successfully and wrote
    /// `config/plot-affordances/generated/<stage_id>.yaml`. `version`
    /// is the version string embedded in the YAML (e.g. `"1.0.0"`).
    PromotedGeneratedRenderer {
        /// Proposal id.
        proposal_id: String,
        /// Stage id the YAML was written for.
        target_stage_id: StageId,
        /// Version string embedded in the promoted YAML.
        version: String,
    },
    /// SME confirmed or rejected an inserted adapter
    /// surfaced by the AdapterWarningCard. Risky / lossy adapters
    /// require explicit acknowledgment before the planner permits
    /// their composition to advance to ValidatedExecutableDag.
    AdapterDecisionRecorded {
        /// Adapter task node id.
        adapter_id: String,
        /// `confirmed` or `rejected`.
        decision: String,
        /// Adapter safety class at decision time (`lossy_declared`,
        /// `scientifically_risky`, `policy_restricted`).
        safety: String,
    },
    /// SME accepted a `NovelNodeSpec` outcome from the
    /// v4 planner (the LLM proposed a hypothesized node and the
    /// SME explicitly authorizes it as a draft) or rejected it.
    NovelNodeDecisionRecorded {
        /// HypothesizedNode id from the proposals registry.
        node_id: String,
        /// `accepted_as_draft` or `rejected`.
        decision: String,
    },
    /// SME acknowledged a `Refusal` outcome and chose
    /// a recovery affordance (branch / amend-policy / dismiss).
    /// Persists which path the SME took so multi-person handoff
    /// has a searchable trail.
    RefusalAcknowledged {
        /// `RefusalReport.id`.
        refusal_id: String,
        /// `branch`, `amend_policy`, or `dismiss`.
        recovery: String,
    },
    /// A later confirmation contradicted an earlier one on the
    /// same assumption.
    AssumptionContradicted {
        /// Assumption id.
        assumption_id: String,
        /// Prior confirmation id.
        prior_confirmation_id: String,
        /// Conflicting confirmation id.
        conflicting_confirmation_id: String,
    },
    /// SME waived an assumption under the assumption-policy table.
    /// V3 §10.2 `policy_rule_id` is the registry-validated
    /// foreign key; deserialization is permissive so legacy decision
    /// logs round-trip unchanged.
    AssumptionWaived {
        /// Assumption id.
        assumption_id: String,
        /// Policy rule id.
        policy_rule_id: PolicyRuleId,
        /// Rationale.
        rationale: String,
        /// Credentials.
        credentials: Vec<String>,
    },
    /// Upstream contract change invalidated a prior resolution.
    /// Forces the resolution back to `Unresolved` on the next planner pass.
    AssumptionInvalidated {
        /// Assumption id.
        assumption_id: String,
        /// Upstream change.
        upstream_change: String,
    },
    /// V3 one of the six non-monotonic lifecycle edges
    /// from design §7 fired and was queued on the session
    /// adjudication queue. `transition_kind` is the
    /// `LifecycleTransition::kind()` discriminator; `payload` is
    /// the JSON-encoded full `LifecycleTransition` so replay can
    /// reconstruct the queued entry without consulting the
    /// session-state file.
    LifecycleTransition {
        /// Transition kind.
        transition_kind: String,
        /// Payload.
        payload: String,
    },
    /// V3 a contradiction (same-user or cross-user) was
    /// detected against a previously confirmed assumption.
    ContradictionDetected {
        /// Assumption id.
        assumption_id: String,
        /// Prior id.
        prior_id: String,
        /// New id.
        new_id: String,
    },
    /// V3 an upstream-invalidation cascade fired.
    /// Captures the cascade-level summary so the audit replay has
    /// a single row for the cascade event rather than N
    /// per-assumption rows.
    InvalidationCascaded {
        /// Assumption id.
        assumption_id: String,
        /// Affected.
        affected: Vec<String>,
    },
    /// The SME rejected a
    /// hypothesized-node proposal at a non-terminal lifecycle stage via the
    /// `POST /api/chat/session/:id/proposal/:proposal_id/reject` endpoint.
    /// Captures the rationale supplied at reject time so the Decisions tab
    /// preserves the audit trail without scanning the session-state file.
    ProposalRejected {
        /// Stable `ProposalId` of the rejected proposal.
        proposal_id: String,
        /// Free-form SME rationale; absent when the reject button was
        /// pressed without a comment.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        rationale: Option<String>,
    },
    /// Atom-safety-policy the SME widened an atom's
    /// `runtime_packages` set from the BlockerCard's `ProvisioningDenied`
    /// affordance, allowing a previously-refused package install on
    /// retry. `atom_id` matches `Task::source_atom_id`; `package` /
    /// `registry` mirror the `ProvisioningDenied` blocker payload that
    /// triggered the affordance. Always idempotent on the session field;
    /// duplicate clicks still write one record per call so the audit
    /// trail captures every SME interaction.
    RuntimePackageAdded {
        /// Atom whose `runtime_packages` set was widened.
        atom_id: AtomId,
        /// Package the SME approved (e.g. `samtools`, `scanpy>=1.10`).
        package: String,
        /// Registry the package is installed from (e.g. `apt`, `pip`,
        /// `cran`, `conda`).
        registry: String,
    },
    /// The server-side claim-verification endpoint
    /// (`POST /api/chat/session/:id/task/:task_id/verify`) re-runs
    /// `claim_extractor` + `claim_verifier` over a completed task's
    /// narrative artifact and appends one row regardless of outcome.
    /// Without this audit record, a `Blocked { ValidationFailed }`
    /// transition (and an SSE event) leaves no trace in
    /// `runtime/decisions.jsonl` — the Decisions tab never sees the
    /// verifier round-trip. The row captures the number of claims
    /// verified, the number that mismatched, and the task the
    /// verifier ran against.
    ClaimVerification {
        /// Task id.
        task_id: TaskId,
        /// N verified.
        n_verified: usize,
        /// N mismatch.
        n_mismatch: usize,
    },
}

/// One audit-trail entry. Append-only; the emitter writes these into
/// `runtime/decisions.jsonl` one record per line.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS, schemars::JsonSchema)]
#[ts(export)]
pub struct DecisionRecord {
    /// On-disk schema version. `#[serde(default)]` gives `0.1.0` to
    /// records written before this field was added, so pre-existing
    /// `decisions.jsonl` files continue to load unchanged.
    #[serde(
        default = "default_decision_record_schema_version",
        with = "crate::migration::schema_version_serde"
    )]
    #[ts(skip)]
    #[schemars(with = "String")]
    pub schema_version: semver::Version,
    /// ISO-8601 UTC timestamp when the decision fired.
    #[ts(type = "string")]
    pub timestamp: DateTime<Utc>,
    /// The session the decision was recorded against. For branch
    /// decisions this is the *parent*; the child session's own log
    /// starts fresh.
    pub session_id: String,
    /// Decision.
    pub decision: DecisionType,
    /// Free-form SME-supplied justification. Optional — the LLM-driven
    /// paths (`emit_package`, `amend_stage_method`) do not always carry
    /// one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub rationale: Option<String>,
    /// Actor.
    pub actor: DecisionActor,
    /// Chain-of-custody for the record when its payload
    /// carries suppressed content. `None` for ordinary records.
    /// Older records without the field deserialize cleanly.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub chain_of_custody: Option<ChainOfCustody>,
    /// Closes the security-remediation gap "no audit logging of SME
    /// action provenance."
    /// Best-effort capture of the client IP that originated the
    /// decision, populated from the request's `X-Forwarded-For`
    /// header (when present and the operator's reverse proxy is
    /// trusted) or the per-connection `ConnectInfo<SocketAddr>`
    /// extension. `None` when the call site has not yet been wired
    /// to extract a request address (LLM-side tool dispatch,
    /// harness-side decision recording, scripted offline tests).
    /// The audit deliverable is that the field is AVAILABLE in the
    /// type; downstream call sites can backfill the value
    /// incrementally without breaking the on-disk JSONL format —
    /// the `#[serde(default, skip_serializing_if = "Option::is_none")]`
    /// guard keeps every legacy record round-trippable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub source_ip: Option<String>,
}

impl DecisionRecord {
    /// New.
    pub fn new(
        session_id: impl Into<String>,
        decision: DecisionType,
        actor: DecisionActor,
        rationale: Option<String>,
    ) -> Self {
        Self {
            schema_version: current_decision_record_version(),
            timestamp: Utc::now(),
            session_id: session_id.into(),
            decision,
            rationale,
            actor,
            chain_of_custody: None,
            source_ip: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Round-trip every variant. Guards against accidental drops from the
    /// internally-tagged serde coverage.
    #[test]
    fn every_decision_variant_roundtrips() {
        let cases = vec![
            DecisionType::Confirm { summary_hash: None },
            DecisionType::Reject,
            DecisionType::Unblock,
            DecisionType::Branch {
                child_session_id: "11111111-1111-1111-1111-111111111111".into(),
            },
            DecisionType::EmitPackage {
                output_dir: "/tmp/pkg".into(),
            },
            DecisionType::AmendStage {
                stage: "integration".into(),
                method_prose: "use Harmony instead of CCA".into(),
            },
            DecisionType::RerunTask {
                task_id: "qc_preprocessing".into(),
                reason: Some("input data refreshed".into()),
            },
            DecisionType::SelectSensitivityWinner {
                stage: "sensitivity_integration".into(),
                winner: "scANVI".into(),
                rejected_candidates: vec!["scVI".into(), "harmony".into()],
            },
            DecisionType::CrossVersionDiff {
                parent_package: "/tmp/pkg-v1".into(),
                child_package: "/tmp/pkg-v2".into(),
                overall_concordance: 0.87,
                n_discordant: 3,
            },
            DecisionType::PostHocDeviation {
                target_stage: "primary_endpoint".into(),
                prior_method: "MMRM".into(),
                new_method: "CMH stratified".into(),
                reason: "site imbalance in primary analysis set".into(),
            },
            DecisionType::AutoAdvanced {
                stage: "subgroup_analyses".into(),
                mode: "fast".into(),
            },
            DecisionType::AppliedStructuredDecision {
                task_id: "biological_interpretation".into(),
                decision_point_id: "interpretation_runtime_substitution".into(),
                chosen: "authorise_gseapy_substitution".into(),
            },
            DecisionType::DispositionProposed {
                path: "runtime/outputs/results_review/sme_disposition.json".into(),
                task_id: "results_review".into(),
                created_at: "2026-04-24T00:09:56Z".into(),
                action_count: 2,
            },
            DecisionType::DispositionApplied {
                path: "runtime/outputs/results_review/sme_disposition.json".into(),
                action_index: 0,
                action_kind: "amend_method".into(),
                target_stage: "batch_correction".into(),
                outcome: "ok".into(),
                error_reason: None,
                auto: false,
            },
            DecisionType::DispositionRejected {
                path: "runtime/outputs/results_review/sme_disposition.json".into(),
                rationale: Some("not ready to re-run the full slice".into()),
            },
            DecisionType::UndoneAmendment {
                stage: "batch_correction".into(),
                reverted_to: "use CCA instead of Harmony".into(),
            },
            DecisionType::BudgetChanged {
                prior_usd: Some(25.0),
                new_usd: Some(50.0),
            },
            DecisionType::UserNote {
                task_id: "qc_preprocessing".into(),
                body: "Re-check the mitochondrial cutoff after the rerun.".into(),
                author: "alan".into(),
            },
            DecisionType::SetIntakeField {
                stage: "qc_preprocessing".into(),
                field: "min_genes_per_cell".into(),
                value: serde_json::json!(200),
            },
            DecisionType::SetIntakeMethod {
                stage: "integration".into(),
                method_prose: "use Harmony with theta=2".into(),
            },
            DecisionType::AppendIntakeProse {
                fragment: "scRNA-seq comparison of liver biopsy samples".into(),
                classified_modality: "single_cell_rnaseq".into(),
                modality_changed: true,
            },
            DecisionType::AssumptionRecorded {
                id: "a_1".into(),
                statement: "Reads assumed unstranded; library prep predates strandedness metadata"
                    .into(),
                source: "llm_inferred".into(),
                affects_nodes: vec!["quantify_features".into()],
                risk: "moderate".into(),
            },
            DecisionType::AssumptionResolved {
                id: "a_1".into(),
                resolution: "accepted".into(),
            },
            DecisionType::ProposedHypothesizedNode {
                node_id: "doublet_score".into(),
                parent_terms: vec!["data:2603".into()],
                // `llm_rationale` is `Option<String>` for legacy
                // on-disk replay only; new records always set `None`.
                llm_rationale: None,
            },
            DecisionType::PlotAffordanceResolved {
                task_id: "differential_expression".into(),
                port_name: "result_table".into(),
                affordance_variant: "registered".into(),
                figure_ids: vec!["volcano".into(), "ma_plot".into()],
                provisional: false,
                snapshot_id: "snap-2026-05-08-a".into(),
            },
            DecisionType::PlotAffordanceFallback {
                task_id: "qc_preprocessing".into(),
                port_name: "qc_metrics".into(),
                primitive: "distribution".into(),
                semantic_type: "data:9999".into(),
                fallback_reason: "no registered renderer; using structural fallback".into(),
            },
            DecisionType::ProposedHypothesizedRenderer {
                proposal_id: "renderer-proposal-abc123def456".into(),
                target_semantic_type: "swfc:custom_volcano".into(),
                sme_intent:
                    "SME wants a volcano plot with highlighted candidate genes and custom axis labels"
                        .into(),
            },
            DecisionType::RendererDraftRequested {
                proposal_id: "renderer-proposal-abc123def456".into(),
                model: "opus_4_7".into(),
            },
            DecisionType::RendererDraftReceived {
                proposal_id: "renderer-proposal-abc123def456".into(),
                lints: vec!["advisory: use _core.savefig".into()],
            },
            DecisionType::RendererSandboxOutcome {
                proposal_id: "renderer-proposal-abc123def456".into(),
                outcome: "static_checks_passed".into(),
            },
            DecisionType::ApproveGeneratedRenderer {
                proposal_id: "renderer-proposal-abc123def456".into(),
                approver: "alan".into(),
            },
            DecisionType::RejectGeneratedRenderer {
                proposal_id: "renderer-proposal-abc123def456".into(),
                reason: "figure does not match SME intent".into(),
            },
            DecisionType::PromotedGeneratedRenderer {
                proposal_id: "renderer-proposal-abc123def456".into(),
                target_stage_id: "custom_volcano".into(),
                version: "1.0.0".into(),
            },
            DecisionType::AdapterDecisionRecorded {
                adapter_id: "lossy_bam_to_cram".into(),
                decision: "confirmed".into(),
                safety: "lossy_declared".into(),
            },
            DecisionType::NovelNodeDecisionRecorded {
                node_id: "doublet_score".into(),
                decision: "accepted_as_draft".into(),
            },
            DecisionType::RefusalAcknowledged {
                refusal_id: "policy_refusal_1".into(),
                recovery: "amend_policy".into(),
            },
            DecisionType::AssumptionContradicted {
                assumption_id: "a_1".into(),
                prior_confirmation_id: "conf_1".into(),
                conflicting_confirmation_id: "conf_2".into(),
            },
            DecisionType::AssumptionWaived {
                assumption_id: "a_1".into(),
                policy_rule_id: PolicyRuleId::unchecked("genome_build_mismatch:research"),
                rationale: "lead approved waiver after manual review".into(),
                credentials: vec!["bioinformatics_lead".into()],
            },
            DecisionType::AssumptionInvalidated {
                assumption_id: "a_1".into(),
                upstream_change: "affects_nodes: [old] → [new]".into(),
            },
            DecisionType::LifecycleTransition {
                transition_kind: "same_user_contradiction".into(),
                payload: r#"{"kind":"same_user_contradiction","actor":"alan","assumption_id":"a_1","prior_record_id":"rec_1","new_record_id":"rec_2"}"#.into(),
            },
            DecisionType::ContradictionDetected {
                assumption_id: "a_1".into(),
                prior_id: "rec_1".into(),
                new_id: "rec_2".into(),
            },
            DecisionType::InvalidationCascaded {
                assumption_id: "a_1".into(),
                affected: vec!["task_1".into(), "task_2".into()],
            },
            DecisionType::ProposalRejected {
                proposal_id: "proposal-abc123def456".into(),
                rationale: Some("changes the input contract we already verified".into()),
            },
            DecisionType::RuntimePackageAdded {
                atom_id: "align_reads".into(),
                package: "samtools".into(),
                registry: "apt".into(),
            },
            DecisionType::ClaimVerification {
                task_id: "differential_expression".into(),
                n_verified: 18,
                n_mismatch: 2,
            },
        ];
        assert_eq!(
            cases.len(),
            <DecisionType as strum::EnumCount>::COUNT,
            "the cases array must enumerate every DecisionType variant; \
             cases={} but DecisionType::COUNT={}",
            cases.len(),
            <DecisionType as strum::EnumCount>::COUNT
        );

        for d in cases {
            let record = DecisionRecord::new(
                "22222222-2222-2222-2222-222222222222",
                d.clone(),
                DecisionActor::Sme,
                Some("because the SME said so".into()),
            );
            let json = serde_json::to_string(&record).expect("serialize");
            let back: DecisionRecord = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(record, back);
        }
    }

    #[test]
    fn actor_roundtrips() {
        for a in [
            DecisionActor::Sme,
            DecisionActor::Llm,
            DecisionActor::Harness,
        ] {
            let json = serde_json::to_string(&a).unwrap();
            let back: DecisionActor = serde_json::from_str(&json).unwrap();
            assert_eq!(a, back);
        }
    }

    /// Rationale is skipped when `None` so the on-disk JSONL stays
    /// compact for the LLM-originated paths.
    #[test]
    fn none_rationale_omitted_from_json() {
        let record = DecisionRecord::new(
            "s1",
            DecisionType::Confirm { summary_hash: None },
            DecisionActor::Sme,
            None,
        );
        let json = serde_json::to_string(&record).unwrap();
        assert!(!json.contains("rationale"), "got: {}", json);
    }

    /// Legacy on-disk `{"kind":"confirm"}` records (which carry no
    /// `summary_hash` field) must continue to deserialize into the
    /// `Confirm { summary_hash: None }` shape. The `#[serde(default)]`
    /// on the field is the load-bearing invariant; this test pins it.
    #[test]
    fn confirm_legacy_unit_variant_round_trips() {
        let legacy = r#"{"kind":"confirm"}"#;
        let parsed: DecisionType = serde_json::from_str(legacy).expect("legacy confirm parses");
        match parsed {
            DecisionType::Confirm { summary_hash } => {
                assert!(
                    summary_hash.is_none(),
                    "legacy unit-form must load as Confirm with summary_hash=None"
                );
            }
            other => panic!("expected Confirm variant, got {other:?}"),
        }
    }

    /// New records carry the SHA-256 fingerprint of the confirmation
    /// summary the SME saw. The on-disk shape keeps the
    /// internally-tagged JSON pattern.
    #[test]
    fn confirm_with_summary_hash_serializes_with_hash_field() {
        let record = DecisionRecord::new(
            "s1",
            DecisionType::Confirm {
                summary_hash: Some("deadbeef".into()),
            },
            DecisionActor::Sme,
            None,
        );
        let json = serde_json::to_string(&record).unwrap();
        assert!(
            json.contains("\"summary_hash\":\"deadbeef\""),
            "summary_hash must be serialized; got {json}"
        );
        let back: DecisionRecord = serde_json::from_str(&json).expect("round-trips");
        assert_eq!(record, back);
    }

    /// The internally-tagged JSON shape is flat.
    #[test]
    fn decision_type_serde_shape_is_flat() {
        let d = DecisionType::AmendStage {
            stage: "s".into(),
            method_prose: "m".into(),
        };
        let v: serde_json::Value = serde_json::to_value(d).unwrap();
        assert_eq!(v["kind"], "amend_stage");
        assert_eq!(v["stage"], "s");
        assert_eq!(v["method_prose"], "m");
    }

    /// `source_ip` is omitted from the serialized record
    /// when unset so legacy on-disk JSONL stays compact and existing
    /// records keep their byte shape.
    #[test]
    fn source_ip_omitted_when_none() {
        let record = DecisionRecord::new(
            "s1",
            DecisionType::Confirm { summary_hash: None },
            DecisionActor::Sme,
            None,
        );
        let json = serde_json::to_string(&record).unwrap();
        assert!(
            !json.contains("source_ip"),
            "source_ip must be skipped when None; got: {json}"
        );
    }

    /// When populated, `source_ip` serializes as a flat
    /// top-level string and round-trips through deserialization.
    #[test]
    fn source_ip_round_trips_when_set() {
        let mut record = DecisionRecord::new("s1", DecisionType::Unblock, DecisionActor::Sme, None);
        record.source_ip = Some("203.0.113.42".into());
        let json = serde_json::to_string(&record).unwrap();
        assert!(
            json.contains("\"source_ip\":\"203.0.113.42\""),
            "source_ip must serialize at top level; got: {json}"
        );
        let back: DecisionRecord = serde_json::from_str(&json).expect("round-trips");
        assert_eq!(record, back);
        assert_eq!(back.source_ip.as_deref(), Some("203.0.113.42"));
    }

    /// Legacy decision records (predating the
    /// `source_ip` field) must continue to deserialize into the new
    /// struct with `source_ip = None`. The `#[serde(default)]` on
    /// the field is the load-bearing invariant; this test pins it.
    #[test]
    fn legacy_record_without_source_ip_deserializes() {
        let legacy = r#"{
            "timestamp": "2026-05-13T18:00:00Z",
            "session_id": "22222222-2222-2222-2222-222222222222",
            "decision": {"kind": "unblock"},
            "actor": "sme"
        }"#;
        let parsed: DecisionRecord =
            serde_json::from_str(legacy).expect("legacy record without source_ip parses");
        assert!(
            parsed.source_ip.is_none(),
            "legacy record must load with source_ip = None"
        );
    }

    #[test]
    fn decision_record_round_trips_schema_version() {
        let record = DecisionRecord::new(
            "s1",
            DecisionType::Confirm { summary_hash: None },
            DecisionActor::Sme,
            None,
        );
        let json = serde_json::to_string(&record).unwrap();
        let back: DecisionRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(record.schema_version, back.schema_version);
    }

    #[test]
    fn legacy_decision_record_without_schema_version_loads_with_default() {
        let legacy = r#"{
            "timestamp": "2026-05-13T18:00:00Z",
            "session_id": "22222222-2222-2222-2222-222222222222",
            "decision": {"kind": "unblock"},
            "actor": "sme"
        }"#;
        let parsed: DecisionRecord = serde_json::from_str(legacy)
            .expect("legacy DecisionRecord without schema_version parses");
        assert_eq!(
            parsed.schema_version,
            semver::Version::new(0, 1, 0),
            "missing schema_version must default to 0.1.0"
        );
    }
}
