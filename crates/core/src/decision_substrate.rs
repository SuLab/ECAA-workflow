//! Verifier decision substrate (v4 §10.7 / F18). Supersedes v3 P6's
//! `RejectedProposal`-only audit log with a typed event union that
//! captures every load-bearing decision made by the compatibility
//! engine, the v4 planner, the policy gate, and the LLM-mediated repair
//! loop.
//!
//! ## Why a substrate, not a single log line per decision
//!
//! v3's audit log recorded *rejections* only — the v4 design widens
//! the lens to every verifier choice (proven, refused, ranked,
//! consulted, accepted, rejected, scope-checked) so post-hoc analysis
//! can reconstruct *why* the composer produced this DAG instead of an
//! alternative. The substrate is **append-only**: events accumulate
//! during composition; the emit step drains them and writes one JSON
//! object per line to `runtime/verifier-decisions.jsonl`.
//!
//! ## Buffer pattern (session-scoped)
//!
//! A global static `OnceLock<Mutex<BTreeMap<SessionKey, Vec<_>>>>`
//! partitions events **by session** so two concurrent server sessions
//! composing at the same time never interleave into one shared buffer
//! (which previously let the first session to emit scoop both sessions'
//! decisions). The routing key is an *ambient* per-thread "current
//! session id" set by a [`SessionScope`] RAII guard — this keeps the
//! `record(d)` / `drain()` call sites argument-free (they are invoked
//! from deep inside the compatibility engine, planner, and policy gate
//! where threading a session id through every signature is impractical).
//!
//! - `record(d)` appends into the bucket for the thread's current
//!   session scope (or the unscoped default bucket when no scope is
//!   active).
//! - `drain()` empties the thread's current session scope's bucket; with
//!   no active scope it drains **and merges** every bucket, preserving
//!   the historical "drain everything" semantics for unscoped callers.
//! - `drain_session(id)` empties exactly one session's bucket regardless
//!   of the thread's ambient scope — the isolation-correct entry point
//!   for the emit-time writer.
//!
//! The composer enters a [`SessionScope`] around its top-level `plan()`
//! call (see `composer::dispatch`), so all of a session's compose-time
//! decisions land in that session's bucket. Mutex poisoning is treated
//! as a soft-fail (the substrate is diagnostic, not load-bearing for
//! composition correctness).

use serde::{Deserialize, Serialize};
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::sync::{Mutex, OnceLock};
use ts_rs::TS;

/// One typed verifier decision. The enum is `serde(tag = "kind")` so
/// the on-disk JSONL format reads as `{"kind":"unification_attempted",...}`
/// rows that filter cleanly in `jq`, `grep`, or the UI table.
///
/// New variants are append-only; never reorder or remove existing
/// variants — historical substrate files must continue to round-trip.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, TS, schemars::JsonSchema)]
#[ts(export)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum VerifierDecision {
    /// The compatibility engine entered `prove()` for an edge. Emitted
    /// at function entry so every prove call has at least one substrate
    /// row.
    UnificationAttempted {
        /// Id.
        id: String,
        /// Timestamp.
        timestamp: String,
        /// Producer port.
        producer_port: String,
        /// Consumer port.
        consumer_port: String,
        /// Ctx hash.
        ctx_hash: String,
    },
    /// `prove()` returned an incompatibility report. `reason` carries
    /// the first incompatibility — full report is recoverable from the
    /// proof sidecar if more detail is needed.
    UnificationFailed {
        /// Id.
        id: String,
        /// Timestamp.
        timestamp: String,
        /// Producer port.
        producer_port: String,
        /// Consumer port.
        consumer_port: String,
        /// Reason.
        reason: IncompatibilityReason,
    },
    /// `prove()` returned `Compatible` or `CompatibleWithAdapters`.
    /// `adapters_inserted` is empty for the plain `Compatible` case.
    UnificationSucceeded {
        /// Id.
        id: String,
        /// Timestamp.
        timestamp: String,
        /// Producer port.
        producer_port: String,
        /// Consumer port.
        consumer_port: String,
        /// Proof id.
        proof_id: String,
        /// Adapters inserted.
        adapters_inserted: Vec<String>,
        /// Residual assumptions.
        residual_assumptions: Vec<String>,
    },
    /// One alternative DAG was sorted into the ranked slate by the
    /// planner. `rank` is zero-based; lower is better. `source` is
    /// `"archetype"` or `"search"` today (future planners may add).
    AlternativeRanked {
        /// Id.
        id: String,
        /// Timestamp.
        timestamp: String,
        /// Dag id.
        dag_id: String,
        /// Rank.
        rank: u32,
        /// Source.
        source: String,
        /// Score summary.
        score_summary: String,
    },
    /// An adapter was inserted into the composition. `safety` is one of
    /// `lossless` / `lossy_declared` / `scientifically_risky` /
    /// `policy_restricted`. `rationale` mirrors the proof's facet
    /// rationale string.
    AdapterInserted {
        /// Id.
        id: String,
        /// Timestamp.
        timestamp: String,
        /// Adapter class.
        adapter_class: String,
        /// Safety.
        safety: String,
        /// Producer node.
        producer_node: String,
        /// Consumer node.
        consumer_node: String,
        /// Rationale.
        rationale: String,
    },
    /// The assumption-policy table was consulted at the v3-Phase-2
    /// labelled section in `classify_outcome_with_policy`. One row per
    /// `(defect_class × privacy_class)` lookup.
    AssumptionPolicyConsulted {
        /// Id.
        id: String,
        /// Timestamp.
        timestamp: String,
        /// Defect class.
        defect_class: String,
        /// Privacy class.
        privacy_class: String,
        /// Resolution.
        resolution: String,
        /// Rule id.
        rule_id: String,
    },
    /// The promotion gate (v4 P3) ran for a candidate node transition.
    /// `result` is one of `passed` / `refused`. `missing_classes` lists
    /// the validation classes the node lacks for the target state.
    PromotionGateConsulted {
        /// Id.
        id: String,
        /// Timestamp.
        timestamp: String,
        /// Node id.
        node_id: String,
        /// Target state.
        target_state: String,
        /// Result.
        result: String,
        /// Required classes.
        required_classes: Vec<String>,
        /// Passing classes.
        passing_classes: Vec<String>,
        /// Missing classes.
        missing_classes: Vec<String>,
    },
    /// A proposal was rejected. Supersedes v3 P6's `RejectedProposal`.
    /// `source` identifies who proposed it (LLM tool call, planner
    /// seed, repair strategy, compatibility candidate); `proposal_kind`
    /// is the proposed mutation shape (named with the `proposal_` prefix
    /// to avoid collision with the enum's `kind` serde tag);
    /// `rejected_by` is the component that turned it down.
    ProposalRejected {
        /// Id.
        id: String,
        /// Timestamp.
        timestamp: String,
        /// Source.
        source: ProposalSource,
        #[serde(rename = "proposal_kind")]
        /// Proposal kind.
        proposal_kind: ProposalKind,
        /// Reason.
        reason: RejectionReason,
        /// Rejected by.
        rejected_by: RejectingComponent,
    },
    /// A repair proposal was emitted by v4 P5's repair-strategy module.
    /// `proposal_payload` is the serialized strategy-specific payload
    /// (kept as opaque string so payload schema changes don't churn
    /// the substrate types).
    RepairProposed {
        /// Id.
        id: String,
        /// Timestamp.
        timestamp: String,
        /// Gap id.
        gap_id: String,
        /// Strategy.
        strategy: String,
        /// Risk class.
        risk_class: String,
        /// Proposal payload.
        proposal_payload: String,
    },
    /// A repair proposal was accepted (SME, auto-accept under policy, or
    /// the planner's auto-application path for `LowAutoAttempt`
    /// proposals). `credentials` records the authority chain for waived
    /// risky-adapter decisions.
    ///
    /// `attempt_kind` discriminates the two acceptance origins: `Auto`
    /// means the planner auto-applied a `LowAutoAttempt` proposal during
    /// composition; `Manual` means the SME accepted via the
    /// `/repair/:proposal_id/accept` endpoint. The field is
    /// `#[serde(default)]` for backward compatibility with historical
    /// substrate files emitted before the field existed.
    ///
    /// `applied_modification` carries the exact `DagModification` payload
    /// that was spliced into the DAG at apply time — `None` for manual
    /// accepts where mutation happens out-of-band, `Some` for auto-applied
    /// repairs so the substrate captures the literal mutation. Stored as a
    /// JSON string so adding new `DagModification` variants doesn't churn
    /// the substrate schema.
    RepairAccepted {
        /// Id.
        id: String,
        /// Timestamp.
        timestamp: String,
        /// Proposal id.
        proposal_id: String,
        /// Acceptor.
        acceptor: String,
        /// Credentials.
        credentials: Vec<String>,
        /// V3+v4 residuals `Auto` vs `Manual` acceptance
        /// origin. Defaults to `Manual` for historical session replay
        /// (older sessions only saw the SME accept path, so `Manual` is
        /// the correct historical default).
        #[serde(default)]
        attempt_kind: AttemptKind,
        /// V3+v4 residuals exact mutation applied to the DAG
        /// when `attempt_kind == Auto`. JSON-encoded `DagModification`
        /// kept opaque so payload-schema churn doesn't break this
        /// variant's wire shape. `None` for `Manual` accepts where the
        /// DAG mutation happens via the planner re-run after the SME
        /// accept endpoint.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        applied_modification: Option<String>,
    },
    /// A repair proposal was rejected.
    RepairRejected {
        /// Id.
        id: String,
        /// Timestamp.
        timestamp: String,
        /// Proposal id.
        proposal_id: String,
        /// Reason.
        reason: String,
    },
    /// The modality-ontology coverage matrix (v4 P1) was consulted at
    /// the LocalExtension parent-term scope check site in
    /// `compatibility/engine.rs::prove`. `result` is one of
    /// `in_primary` / `in_secondary` / `forbidden` / `out_of_scope`.
    OntologyScopeChecked {
        /// Id.
        id: String,
        /// Timestamp.
        timestamp: String,
        /// Modality.
        modality: String,
        /// Candidate iri.
        candidate_iri: String,
        /// Result.
        result: String,
        /// Rule id.
        rule_id: String,
    },
    /// v3 P8 follow-up — a non-monotonic lifecycle edge from
    /// `crate::lifecycle_adversarial::LifecycleTransition` was
    /// detected by `detect_lifecycle_adversarial_edges` (or one of
    /// its sub-detectors). The substrate carries the same payload as
    /// the decision-log + adjudication-queue write so replay can
    /// reconstruct the lifecycle drama without joining three sidecars.
    ///
    /// `transition_kind` is the snake_case discriminator returned by
    /// `LifecycleTransition::kind()` (e.g. `"same_user_contradiction"`,
    /// `"production_node_revocation"`); `affected_node_id` carries the
    /// primary node/assumption id from the transition payload (so a
    /// `grep` of the substrate file finds every event touching a
    /// given node); `rationale` is a short narrative the UI's
    /// `LifecycleAdjudicationCard` can render without unpacking the
    /// full payload.
    LifecycleAdversarialEdgeDetected {
        /// Id.
        id: String,
        /// Timestamp.
        timestamp: String,
        /// Transition kind.
        transition_kind: String,
        /// Affected node id.
        affected_node_id: String,
        /// Rationale.
        rationale: String,
    },
    /// v3 P8 follow-up — an `AdjudicationQueueEntry` was appended to
    /// `Session::adjudication_queue` by `enqueue_adjudication`.
    /// Paired one-to-one with a prior
    /// `LifecycleAdversarialEdgeDetected` row (same `transition_kind`)
    /// so the F18 substrate-completeness property test can assert
    /// `queue_writes == substrate_enqueues`.
    AdjudicationEnqueued {
        /// Id.
        id: String,
        /// Timestamp.
        timestamp: String,
        /// Queue entry id.
        queue_entry_id: String,
        /// Transition kind.
        transition_kind: String,
    },
    /// A composed-DAG edge that did not prove a typed data flow was
    /// waved through as workflow-ordering-only. Recorded once per
    /// `EdgeKind::OrderingOnly` classification so an auditor can see
    /// which edges entered an executable DAG untyped. `declared`
    /// distinguishes an archetype-author exemption from a synthesis-
    /// site ordering edge; `risk_mode` records the strictness band in
    /// force (an OrderingOnly edge in Production would have rejected).
    OrderingEdgeExempted {
        /// Id.
        id: String,
        /// Timestamp.
        timestamp: String,
        /// Producer node id.
        producer_node: String,
        /// Consumer node id.
        consumer_node: String,
        /// True when the exemption came from an archetype
        /// `ordering_only_edges` declaration; false for synthesis-site
        /// ordering edges.
        declared: bool,
        /// Strictness band string (`"draft"` / `"production"`).
        risk_mode: String,
    },
}

/// V3+v4 residuals origin discriminator for a
/// `VerifierDecision::RepairAccepted` event. `Auto` rows are emitted by
/// the planner's auto-application path for `LowAutoAttempt` proposals
/// (mutation happens during composition); `Manual` rows are emitted by
/// the `/api/chat/session/:id/repair/:proposal_id/accept` server route
/// after the SME presents the appropriate credentials.
///
/// `Default` is `Manual` so historical substrate files (which only ever
/// carried SME-accept rows) deserialize into the variant whose
/// invariant they actually satisfy. The auto-apply path always sets
/// `Auto` explicitly.
#[derive(
    Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq, TS, schemars::JsonSchema,
)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum AttemptKind {
    /// Planner auto-applied a `LowAutoAttempt` proposal during
    /// composition. The substrate row carries the exact
    /// `DagModification` payload in `applied_modification`.
    Auto,
    /// SME accepted via the `accept` endpoint. The DAG mutation
    /// happens when the planner re-runs with the proposal applied;
    /// `applied_modification` is `None` on this branch.
    #[default]
    Manual,
}

/// Who proposed a mutation. The four variants cover the LLM mediation
/// surface plus the three deterministic proposer sources inside the
/// composer.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, TS, schemars::JsonSchema)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum ProposalSource {
    /// LlmToolCall variant.
    LlmToolCall,
    /// CompatibilityCandidate variant.
    CompatibilityCandidate,
    /// PlannerSeed variant.
    PlannerSeed,
    /// RepairStrategy variant.
    RepairStrategy,
}

/// The five proposal shapes the composer accepts. Each maps to a
/// specific mutation on the in-flight DAG.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, TS, schemars::JsonSchema)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum ProposalKind {
    /// NodeAddition variant.
    NodeAddition,
    /// EdgeAddition variant.
    EdgeAddition,
    /// NodeReplacement variant.
    NodeReplacement,
    /// ContractMutation variant.
    ContractMutation,
    /// AssumptionResolution variant.
    AssumptionResolution,
}

/// Typed rejection reasons. Free-text fits inside `Other` so the typed
/// vocabulary stays stable while the substrate keeps every refusal
/// recoverable.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, TS, schemars::JsonSchema)]
#[ts(export)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RejectionReason {
    /// The proposal would create a cycle (F1).
    CycleIntroduction,
    /// The proposal would leave a required input unsatisfied (F2/F3).
    RequiredInputUnsatisfied { port: String },
    /// The proposal failed semantic-type or facet compatibility on
    /// the compatibility engine.
    IncompatibleSemanticType { producer: String, consumer: String },
    /// The proposal was refused by an active policy bundle (F15).
    PolicyViolation {
        /// Bundle id.
        bundle_id: String,
        /// Check kind.
        check_kind: String,
    },
    /// The proposal failed the per-node policy gate (validated-nodes,
    /// pinned containers, etc.).
    PerNodePolicy { node_id: String, check_kind: String },
    /// The proposal failed schema validation (F14).
    SchemaInvalid { statement: String },
    /// Catch-all for site-local or future reasons.
    Other { statement: String },
}

/// Which component issued the rejection. The six variants partition
/// the verifier surface so post-hoc filtering can attribute refusals
/// to a single subsystem.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, TS, schemars::JsonSchema)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum RejectingComponent {
    /// CompatibilityEngine variant.
    CompatibilityEngine,
    /// PolicyGate variant.
    PolicyGate,
    /// PromotionGate variant.
    PromotionGate,
    /// Planner variant.
    Planner,
    /// SchemaValidator variant.
    SchemaValidator,
    /// SiteLocal variant.
    SiteLocal,
}

/// Why a compatibility unification failed. Mirrors the variants of
/// `crate::compatibility::reports::IncompatibilityReason` but lives in
/// the substrate's own type so the substrate file is decoupled from
/// the engine's exact field layout (substrate consumers should not
/// have to reach into the engine module). Each variant carries the
/// minimum fields needed to reconstruct the failure intent.
///
/// `ts-rs` renames the export to `SubstrateIncompatibilityReason` so
/// the binding file doesn't collide with the engine's same-named type.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, TS, schemars::JsonSchema)]
#[ts(export, rename = "SubstrateIncompatibilityReason")]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum IncompatibilityReason {
    /// SemanticTypeMismatch variant.
    SemanticTypeMismatch {
        /// Producer.
        producer: String,
        /// Consumer.
        consumer: String,
    },
    /// FacetMismatch variant.
    FacetMismatch {
        /// Facet.
        facet: String,
        /// Producer.
        producer: String,
        /// Consumer.
        consumer: String,
        /// Rationale.
        rationale: String,
    },
    /// PrivacyClassWidening variant.
    PrivacyClassWidening {
        /// Producer.
        producer: String,
        /// Consumer.
        consumer: String,
    },
    /// CardinalityMismatch variant.
    CardinalityMismatch {
        /// Producer.
        producer: String,
        /// Consumer.
        consumer: String,
    },
    /// PolicyViolation variant.
    PolicyViolation {
        /// Bundle id.
        bundle_id: String,
        /// Check kind.
        check_kind: String,
        /// Statement.
        statement: String,
    },
    /// Other variant.
    Other {
        /// Statement.
        statement: String,
    },
}

impl IncompatibilityReason {
    /// Lift an engine-level [`crate::compatibility::reports::IncompatibilityReason`]
    /// into the substrate's matching variant. Centralizing the mapping
    /// Keeps emission sites concise (`record(... UnificationFailed { reason: IncompatibilityReason::from_engine(r),... })`).
    pub fn from_engine(reason: &crate::compatibility::reports::IncompatibilityReason) -> Self {
        use crate::compatibility::reports::IncompatibilityReason as Engine;
        match reason {
            Engine::SemanticTypeMismatch { producer, consumer } => Self::SemanticTypeMismatch {
                producer: producer.clone(),
                consumer: consumer.clone(),
            },
            Engine::FacetMismatch {
                facet,
                producer,
                consumer,
                rationale,
            } => Self::FacetMismatch {
                facet: facet.clone(),
                producer: producer.clone(),
                consumer: consumer.clone(),
                rationale: rationale.clone(),
            },
            Engine::PrivacyClassWidening { producer, consumer } => Self::PrivacyClassWidening {
                producer: producer.clone(),
                consumer: consumer.clone(),
            },
            Engine::CardinalityMismatch { producer, consumer } => Self::CardinalityMismatch {
                producer: producer.clone(),
                consumer: consumer.clone(),
            },
            Engine::PolicyViolation {
                bundle_id,
                check_kind,
                statement,
            } => Self::PolicyViolation {
                bundle_id: bundle_id.clone(),
                check_kind: check_kind.clone(),
                statement: statement.clone(),
            },
            Engine::Other { statement } => Self::Other {
                statement: statement.clone(),
            },
        }
    }
}

// ---------------------------------------------------------------------
// Buffer surface
// ---------------------------------------------------------------------

/// Routing key for the per-session buffer map. `None` is the unscoped
/// default bucket used by callers that never enter a [`SessionScope`]
/// (today: the server's repair-proposal endpoints, the `unblock`
/// breadcrumb path, and the in-crate property/unit tests, which all
/// serialize through their own mutex and so are not cross-session
/// concurrent on this bucket).
type SessionKey = Option<String>;

/// Session-keyed buffer. `OnceLock<Mutex<BTreeMap<_>>>` so we never need
/// a `static_init`-style crate dependency, the map is `Send + Sync`-safe,
/// and concurrent sessions on different OS threads never share a `Vec`.
/// `BTreeMap` (not `HashMap`) keeps the merge-all `drain()` order stable
/// for the determinism contract.
static BUFFER: OnceLock<Mutex<BTreeMap<SessionKey, Vec<VerifierDecision>>>> = OnceLock::new();

thread_local! {
    /// Ambient "current session id" for this thread. Set by the RAII
    /// [`SessionScope`] guard around the composer's `plan()` call so
    /// every `record()` fired transitively from the engine / planner /
    /// policy gate routes into the right session bucket without
    /// threading the id through every call signature. Defaults to
    /// `None` (the unscoped default bucket).
    static CURRENT_SESSION: RefCell<SessionKey> = const { RefCell::new(None) };
}

fn buffer() -> &'static Mutex<BTreeMap<SessionKey, Vec<VerifierDecision>>> {
    BUFFER.get_or_init(|| Mutex::new(BTreeMap::new()))
}

/// Read the thread's current session scope (a clone of the key).
fn current_session_key() -> SessionKey {
    CURRENT_SESSION.with(|c| c.borrow().clone())
}

/// RAII guard that sets the thread-local "current session" for the
/// duration of a composition and restores the previous value on drop
/// (so nested or sequential compositions on the same thread don't leak
/// scope into each other). Returned by [`enter_session`].
///
/// The guard is intentionally `#[must_use]` and not `Clone`/`Copy`: the
/// scope is tied to the lexical span of the composition.
#[must_use = "the session scope ends when this guard is dropped; bind it to a local"]
pub struct SessionScope {
    /// The scope value that was active before this guard installed its
    /// own, restored verbatim on drop.
    previous: SessionKey,
}

impl Drop for SessionScope {
    fn drop(&mut self) {
        let prev = self.previous.take();
        CURRENT_SESSION.with(|c| *c.borrow_mut() = prev);
    }
}

/// Enter a session scope for the current thread. Every `record()` fired
/// on this thread until the returned [`SessionScope`] drops routes into
/// the `session_id` bucket; the matching [`drain_session`] (or a scoped
/// `drain()`) recovers exactly those rows. Restores the previous scope
/// on drop.
///
/// Composer entry points wrap their top-level `plan()` call in this
/// guard so a session's compose-time decisions are isolated from any
/// other session composing concurrently on a different thread.
pub fn enter_session(session_id: impl Into<String>) -> SessionScope {
    let previous = CURRENT_SESSION.with(|c| c.replace(Some(session_id.into())));
    SessionScope { previous }
}

/// Append a verifier decision into the current thread's session bucket
/// (or the unscoped default bucket when no [`SessionScope`] is active).
/// Mutex poisoning is treated as a soft-fail — the substrate is
/// observational, not load-bearing for composition correctness, and
/// panicking from a logging helper would mask the underlying defect the
/// substrate exists to capture.
pub fn record(d: VerifierDecision) {
    let key = current_session_key();
    if let Ok(mut g) = buffer().lock() {
        g.entry(key).or_default().push(d);
    }
}

/// Drain decisions in insertion order. When a [`SessionScope`] is active
/// on this thread, only that session's bucket is emptied — the
/// isolation-correct behavior for a scoped compose+emit flow. When no
/// scope is active, every bucket is drained and merged in session-key
/// order (the historical "drain everything" semantics that unscoped
/// callers — the emit-time writer, the server repair endpoints, and the
/// in-crate tests — depend on). Mutex poisoning yields an empty Vec.
///
/// For session-isolated emission regardless of the thread's ambient
/// scope, prefer [`drain_session`].
pub fn drain() -> Vec<VerifierDecision> {
    let key = current_session_key();
    let Ok(mut map) = buffer().lock() else {
        return Vec::new();
    };
    match key {
        Some(_) => map.remove(&key).unwrap_or_default(),
        None => {
            // Unscoped: drain + merge every bucket. `BTreeMap::into_values`
            // yields buckets in stable session-key order, and each bucket
            // preserves its own insertion order, so the merged result is
            // deterministic.
            std::mem::take(&mut *map).into_values().flatten().collect()
        }
    }
}

/// Drain exactly the `session_id` bucket in insertion order, regardless
/// of the calling thread's ambient [`SessionScope`]. This is the
/// isolation-correct entry point for the emit-time writer: a session's
/// substrate file gets only that session's decisions even when a sibling
/// session's compose-time rows are still buffered in the process. Mutex
/// poisoning yields an empty Vec.
pub fn drain_session(session_id: &str) -> Vec<VerifierDecision> {
    let Ok(mut map) = buffer().lock() else {
        return Vec::new();
    };
    map.remove(&Some(session_id.to_string())).unwrap_or_default()
}

/// Test/library callers that need to peek at the current thread's
/// session-bucket length without draining (e.g. property tests that
/// assert "at least one event after prove()" but want subsequent
/// assertions to see the same events). Counts the bucket for the
/// thread's active scope, or — when unscoped — the sum across every
/// bucket (mirroring the unscoped `drain()` merge semantics). Soft-fail
/// to 0 on poisoning.
#[doc(hidden)]
pub fn len() -> usize {
    let key = current_session_key();
    match buffer().lock() {
        Ok(map) => match key {
            Some(_) => map.get(&key).map(|v| v.len()).unwrap_or(0),
            None => map.values().map(|v| v.len()).sum(),
        },
        Err(_) => 0,
    }
}

/// Stable timestamp helper. Today returns a placeholder so the emit-time
/// substrate file is byte-deterministic across re-emissions (CLAUDE.md's
/// deterministic-output rule: timestamps live in the documented-non-
/// deterministic file allowlist; substrate is on that list, but
/// determinism replay tests still need stable strings).
///
/// Future refinement: replace with a session-scoped logical clock that
/// increments per emission, so two events from the same session have
/// strictly-ordered timestamps without using wall-clock.
pub fn timestamp() -> String {
    "0".to_string()
}

/// Stable id helper. Combines a `kind` namespace and two stringly-
/// keyed parts to produce a `<kind>:<part_a>:<part_b>` id. Used by
/// the emission sites so substrate ids are recoverable without
/// re-reading the prove/plan input.
pub fn stable_id(kind: &str, part_a: &str, part_b: &str) -> String {
    format!("{kind}:{part_a}:{part_b}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// The substrate buffer is session-keyed but the *unscoped* default
    /// bucket is shared across these tests. Tests that record/drain
    /// without a `SessionScope` serialize against this crate-local guard
    /// so an unscoped merge-all `drain()` in one test doesn't scoop
    /// another's default-bucket rows. Cross-crate serialization isn't
    /// possible without a workspace-level mutex, but cargo runs each
    /// crate's tests in its own binary, so a per-crate guard is
    /// sufficient. Tests that scope into a *unique* session id are
    /// isolated by the session key itself and additionally hold this
    /// guard so a concurrent unscoped merge-all drain can't steal their
    /// scoped rows.
    static SUBSTRATE_GUARD: Mutex<()> = Mutex::new(());

    #[test]
    fn ordering_edge_exempted_round_trips() {
        let d = VerifierDecision::OrderingEdgeExempted {
            id: "ord:p:c".into(),
            timestamp: timestamp(),
            producer_node: "p".into(),
            consumer_node: "c".into(),
            declared: true,
            risk_mode: "draft".into(),
        };
        let json = serde_json::to_string(&d).unwrap();
        assert!(
            json.contains("\"kind\":\"ordering_edge_exempted\""),
            "got {json}"
        );
        let back: VerifierDecision = serde_json::from_str(&json).unwrap();
        assert_eq!(d, back);
    }

    /// A historical substrate row (a pre-existing variant) still
    /// deserializes after the additive variant + #[non_exhaustive].
    #[test]
    fn legacy_unification_attempted_row_still_round_trips() {
        let legacy = r#"{"kind":"unification_attempted","id":"x","timestamp":"0","producer_port":"a","consumer_port":"b","ctx_hash":"h"}"#;
        let back: VerifierDecision = serde_json::from_str(legacy).unwrap();
        assert!(matches!(
            back,
            VerifierDecision::UnificationAttempted { .. }
        ));
    }

    #[test]
    fn record_and_drain_round_trip() {
        let _guard = SUBSTRATE_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        // Drain anything left over from earlier tests in this run.
        // Other test threads (notably the `compatibility::engine`
        // unit tests that exercise `prove()` and thereby emit
        // substrate rows) may have pushed events between the drain
        // and our record; assert on our specific row by id, not on
        // total length.
        let _ = drain();
        let marker_id = format!("u1-test-{}", std::ptr::addr_of!(SUBSTRATE_GUARD) as usize);
        record(VerifierDecision::UnificationAttempted {
            id: marker_id.clone(),
            timestamp: timestamp(),
            producer_port: "p1".into(),
            consumer_port: "c1".into(),
            ctx_hash: "h1".into(),
        });
        let events = drain();
        let found = events.iter().any(|e| {
            matches!(
                e,
                VerifierDecision::UnificationAttempted { id, .. } if *id == marker_id
            )
        });
        assert!(
            found,
            "expected our recorded row (id={marker_id}) in {} events",
            events.len()
        );
    }

    #[test]
    fn proposal_rejected_serializes_with_tagged_kind() {
        let pr = VerifierDecision::ProposalRejected {
            id: "pr1".into(),
            timestamp: "0".into(),
            source: ProposalSource::LlmToolCall,
            proposal_kind: ProposalKind::NodeAddition,
            reason: RejectionReason::CycleIntroduction,
            rejected_by: RejectingComponent::Planner,
        };
        let json = serde_json::to_string(&pr).expect("serialize");
        assert!(json.contains(r#""kind":"proposal_rejected""#), "got {json}");
        assert!(json.contains(r#""source":"llm_tool_call""#), "got {json}");
        assert!(
            json.contains(r#""proposal_kind":"node_addition""#),
            "got {json}"
        );
        let round_trip: VerifierDecision = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(pr, round_trip);
    }

    #[test]
    fn incompatibility_reason_lifts_from_engine() {
        use crate::compatibility::reports::IncompatibilityReason as Engine;
        let engine = Engine::FacetMismatch {
            facet: "genome_build".into(),
            producer: "GRCh37".into(),
            consumer: "GRCh38".into(),
            rationale: "build axis mismatch".into(),
        };
        let lifted = IncompatibilityReason::from_engine(&engine);
        match lifted {
            IncompatibilityReason::FacetMismatch {
                facet, rationale, ..
            } => {
                assert_eq!(facet, "genome_build");
                assert_eq!(rationale, "build axis mismatch");
            }
            other => panic!("expected FacetMismatch, got {:?}", other),
        }
    }

    /// A scoped `drain()` empties only the active session's bucket and
    /// leaves a sibling session's rows untouched; `drain_session`
    /// recovers the sibling's rows on the same thread regardless of the
    /// ambient scope.
    #[test]
    fn scoped_drain_isolates_per_session() {
        let _guard = SUBSTRATE_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        let a = format!("sess-A-{}", std::ptr::addr_of!(SUBSTRATE_GUARD) as usize);
        let b = format!("sess-B-{}", std::ptr::addr_of!(SUBSTRATE_GUARD) as usize);
        // Clear any residue for these (unique) keys.
        let _ = drain_session(&a);
        let _ = drain_session(&b);

        {
            let _scope_a = enter_session(a.clone());
            record(VerifierDecision::UnificationAttempted {
                id: "a-only".into(),
                timestamp: timestamp(),
                producer_port: "pa".into(),
                consumer_port: "ca".into(),
                ctx_hash: "ha".into(),
            });
        }
        {
            let _scope_b = enter_session(b.clone());
            record(VerifierDecision::UnificationAttempted {
                id: "b-only".into(),
                timestamp: timestamp(),
                producer_port: "pb".into(),
                consumer_port: "cb".into(),
                ctx_hash: "hb".into(),
            });
        }

        // Scoped drain of A sees only A's row.
        let drained_a = {
            let _scope_a = enter_session(a.clone());
            drain()
        };
        assert_eq!(drained_a.len(), 1, "A scope must drain only A's row");
        assert!(matches!(
            &drained_a[0],
            VerifierDecision::UnificationAttempted { id, .. } if id == "a-only"
        ));

        // B's bucket is untouched; recover it via drain_session.
        let drained_b = drain_session(&b);
        assert_eq!(drained_b.len(), 1, "B's bucket must survive A's drain");
        assert!(matches!(
            &drained_b[0],
            VerifierDecision::UnificationAttempted { id, .. } if id == "b-only"
        ));
    }

    /// Two concurrent "sessions" on separate OS threads — each entering
    /// its own [`SessionScope`] and recording N rows — must drain only
    /// their own decisions. This is the core-01 regression: before the
    /// session-keyed buffer, both threads pushed into one shared `Vec`
    /// and the first thread to drain scooped the other thread's rows.
    #[test]
    fn concurrent_sessions_drain_only_their_own_decisions() {
        use std::sync::Barrier;
        use std::thread;

        let _guard = SUBSTRATE_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        let tag = std::ptr::addr_of!(SUBSTRATE_GUARD) as usize;
        let sess_a = format!("concurrent-A-{tag}");
        let sess_b = format!("concurrent-B-{tag}");
        let _ = drain_session(&sess_a);
        let _ = drain_session(&sess_b);

        const ROWS: usize = 200;
        // A barrier forces both threads to interleave their record()
        // calls, maximizing the chance a shared-Vec implementation would
        // cross-contaminate.
        let barrier = std::sync::Arc::new(Barrier::new(2));

        let spawn = |session: String, prefix: &'static str| {
            let barrier = barrier.clone();
            thread::spawn(move || {
                let _scope = enter_session(session);
                barrier.wait();
                for i in 0..ROWS {
                    record(VerifierDecision::UnificationAttempted {
                        id: format!("{prefix}-{i}"),
                        timestamp: timestamp(),
                        producer_port: prefix.into(),
                        consumer_port: prefix.into(),
                        ctx_hash: format!("{i}"),
                    });
                }
                // Drain inside the scope so we exercise the scoped
                // drain() path that the emit-time writer's
                // session-isolated entry mirrors.
                drain()
            })
        };

        let ha = spawn(sess_a.clone(), "AAA");
        let hb = spawn(sess_b.clone(), "BBB");
        let drained_a = ha.join().expect("thread A joins");
        let drained_b = hb.join().expect("thread B joins");

        assert_eq!(drained_a.len(), ROWS, "A must drain exactly its rows");
        assert_eq!(drained_b.len(), ROWS, "B must drain exactly its rows");
        assert!(
            drained_a.iter().all(|e| matches!(
                e,
                VerifierDecision::UnificationAttempted { id, .. } if id.starts_with("AAA-")
            )),
            "A's drain leaked a non-A row (cross-session contamination)"
        );
        assert!(
            drained_b.iter().all(|e| matches!(
                e,
                VerifierDecision::UnificationAttempted { id, .. } if id.starts_with("BBB-")
            )),
            "B's drain leaked a non-B row (cross-session contamination)"
        );

        // Both buckets are now empty.
        assert!(drain_session(&sess_a).is_empty());
        assert!(drain_session(&sess_b).is_empty());
    }
}
