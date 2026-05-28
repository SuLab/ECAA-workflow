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
//! alternative. The substrate is **append-only**: a process-wide
//! `Mutex<Vec<_>>` collects events during composition; the emit step
//! drains the buffer and writes one JSON object per line to
//! `runtime/verifier-decisions.jsonl`.
//!
//! ## Buffer pattern
//!
//! A global static `OnceLock<Mutex<Vec<VerifierDecision>>>` keeps the
//! API call-site-free: `record(d)` from anywhere in core, and the
//! emit-time `drain()` returns the accumulated batch in insertion
//! order. Mutex poisoning is treated as a soft-fail (the substrate is
//! diagnostic, not load-bearing for composition correctness).

use serde::{Deserialize, Serialize};
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

/// Process-wide buffer. Compiled-time `OnceLock<Mutex<Vec<_>>>` so we
/// never need a `static_init`-style crate dependency and the buffer
/// reset on `drain()` is `Send + Sync`-safe.
static BUFFER: OnceLock<Mutex<Vec<VerifierDecision>>> = OnceLock::new();

/// Append a verifier decision. Mutex poisoning is treated as a soft-fail
/// — the substrate is observational, not load-bearing for composition
/// correctness, and panicking from a logging helper would mask the
/// underlying defect the substrate exists to capture.
pub fn record(d: VerifierDecision) {
    let buf = BUFFER.get_or_init(|| Mutex::new(Vec::new()));
    if let Ok(mut g) = buf.lock() {
        g.push(d);
    }
}

/// Drain the buffer and return its contents in insertion order. Called
/// from the emit step so the buffer is empty for the next session in a
/// long-running server process. Mutex poisoning yields an empty Vec —
/// see `record` for the rationale.
pub fn drain() -> Vec<VerifierDecision> {
    let buf = BUFFER.get_or_init(|| Mutex::new(Vec::new()));
    match buf.lock() {
        Ok(mut g) => std::mem::take(&mut *g),
        Err(_) => Vec::new(),
    }
}

/// Test/library callers that need to peek at the buffer's current
/// length without draining (e.g. property tests that assert "at least
/// one event after prove()" but want subsequent assertions to see the
/// same events). Soft-fail to 0 on poisoning.
#[doc(hidden)]
pub fn len() -> usize {
    let buf = BUFFER.get_or_init(|| Mutex::new(Vec::new()));
    match buf.lock() {
        Ok(g) => g.len(),
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

    /// The substrate buffer is process-wide. These tests serialize
    /// against the writer's tests in `crates/conversation` via this
    /// crate-local guard; cross-crate serialization isn't possible
    /// without a workspace-level mutex, but in practice cargo runs
    /// each crate's tests in its own binary, so a per-crate guard
    /// is sufficient.
    static SUBSTRATE_GUARD: Mutex<()> = Mutex::new(());

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
}
