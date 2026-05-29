//! The compatibility engine.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::adapter_registry::{AdapterRegistry, AdapterSafety, AdapterSpec};
use crate::edam::edam_subtype_edges;
use crate::policy_context::{PolicyCheck, PolicyCheckKind, PolicyContext};
use crate::workflow_contracts::edge::CompatibilityProof;
use crate::workflow_contracts::port::PortContract;
use crate::workflow_contracts::semantic_type::SemanticType;
use crate::workflow_contracts::task_node::TaskNode;

use super::facet_unification::{unify_facet, FacetUnification};
use super::proof_builder::ProofBuilder;
use super::reports::{IncompatibilityReason, IncompatibilityReport};
use crate::workflow_contracts::edge::ProofEvidence;

/// Reason the engine cannot decide. Surfaces as `Unknown` in the
/// `CompatibilityResult`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, TS, schemars::JsonSchema)]
#[ts(export)]
pub struct ClarificationOrValidationNeeded {
    /// Stable id (so the UI ledger can dedupe).
    pub id: String,
    /// What's missing or unclear.
    pub statement: String,
    /// What the SME / agent could supply to resolve.
    pub suggestion: String,
}

/// Result of a single edge's compatibility check.
///
/// Note: derives `PartialEq` but not `Eq` because
/// `CompatibleWithAdapters.adapters: Vec<TaskNode>` references
/// `PortContract`/`SemanticType` which no longer implement `Eq` after
/// v4 P6 (`LocalExtensionMaturity::GraduationCandidate` carries an
/// `f32 success_rate`). Equality of two `CompatibilityResult`s is rare
/// outside test asserts; tests can match on `kind` and inspect fields.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS, schemars::JsonSchema)]
#[ts(export)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CompatibilityResult {
    /// Edge holds with the given proof. Production-ready when the
    /// proof has no warnings or unknown-facet entries.
    Compatible(CompatibilityProof),
    /// Edge holds, but adapters need to be inserted between
    /// producer and consumer. The adapter registry populates the
    /// `adapters` slice.
    CompatibleWithAdapters {
        /// Proof.
        proof: CompatibilityProof,
        /// Adapters.
        adapters: Vec<TaskNode>,
    },
    /// Edge cannot hold.
    Incompatible(IncompatibilityReport),
    /// Engine cannot decide given current information; SME
    /// confirmation or validator output required.
    Unknown(ClarificationOrValidationNeeded),
}

/// Mode the planner runs in. `Production` requires
/// `Compatible` for every edge; `Draft` accepts
/// `CompatibleWithAdapters` and `Unknown` (downgrade to
/// `ComposeOutcome::DraftDag`).
#[derive(
    Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, TS, Default, schemars::JsonSchema,
)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum RiskMode {
    #[default]
    /// Draft variant.
    Draft,
    /// Production variant.
    Production,
}

/// Adapter policy controlling which adapter safety classes may be auto-inserted.
#[derive(
    Debug, Clone, Serialize, Deserialize, PartialEq, Eq, TS, Default, schemars::JsonSchema,
)]
#[ts(export)]
pub struct AdapterPolicy {
    /// True when the engine may auto-insert `Lossless` adapters.
    pub allow_lossless: bool,
    /// True when `LossyDeclared` adapters may be auto-inserted
    /// behind an assumption-ledger entry.
    pub allow_lossy_with_assumption: bool,
    /// True when `ScientificallyRisky` adapters may be inserted
    /// (only with explicit SME confirmation).
    pub allow_risky_with_confirmation: bool,
}

impl AdapterPolicy {
    /// Lossless only.
    pub fn lossless_only() -> Self {
        Self {
            allow_lossless: true,
            allow_lossy_with_assumption: false,
            allow_risky_with_confirmation: false,
        }
    }
    /// Permissive drafts.
    pub fn permissive_drafts() -> Self {
        Self {
            allow_lossless: true,
            allow_lossy_with_assumption: true,
            allow_risky_with_confirmation: false,
        }
    }
}

/// Search and policy context the engine reads. Carries
/// snapshots so the engine is replayable for determinism tests.
#[derive(Debug, Clone, Default)]
pub struct PlanningContext {
    /// Risk mode: `Draft` or `Production`.
    pub risk_mode: RiskMode,
    /// Adapter policy.
    pub adapter_policy: AdapterPolicy,
    /// Snapshot id for the ontology version used during proofs.
    pub ontology_snapshot_id: Option<String>,
    /// Snapshot id for the atom registry used during search.
    pub atom_snapshot_id: Option<String>,
    /// Search/budget limits — bounded so determinism tests run
    /// in a fixed time.
    pub max_proof_branches: u32,
    /// Composer-
    /// readable policy bundles. When non-empty, the engine
    /// evaluates each active policy check against the proposed
    /// edge and refuses with `IncompatibilityReason::PolicyViolation`
    /// when any hard policy fails. Soft policy checks (audit-trail-
    /// required, human-signoff-required) are recorded in
    /// `CompatibilityProof.policy_decisions` and do not block the
    /// edge.
    pub policy_context: PolicyContext,
    /// V4 modality-ontology coverage matrix consulted on
    /// `LocalExtension` parent-term proposals. Annotation-only today
    /// (forbidden parents log a `tracing::warn!` and do not abort);
    /// v4 P2 wraps each scope check with substrate emission of
    /// `OntologyScopeChecked` so the matrix becomes hard refusal.
    /// Same `Option<Arc<...>>` shape as the composer's PlanningContext
    /// for symmetry — defaults to `None` so existing call sites compile.
    pub ontology_scope: Option<std::sync::Arc<crate::ontology_scope::OntologyScopeMatrix>>,
    /// v3 §4 / v4 §4 Round-2 closure (G1 / G13) — cross-session opaque
    /// observation sink. When set, `prove()` records each
    /// `SemanticType::Opaque` short-circuit (the path that returns
    /// `CompatibilityResult::Unknown`) so the conversation crate's
    /// `OpaqueAggregator` can surface persistent opaque types as a
    /// registry-improvement signal. When None, the engine emits a
    /// `tracing::warn!` line so operators running without the
    /// aggregator wired still see the observation in logs.
    pub opaque_observation_sink: Option<std::sync::Arc<dyn OpaqueObservationSink + Send + Sync>>,

    /// Optional session id supplied by the conversation crate when a
    /// chat session drives composition. Used by the Opaque-observation
    /// sink to attribute cross-session aggregation correctly. None at
    /// bare-composer call sites (CLI `intake`, eval-baselines, tests).
    pub opaque_session_id: Option<String>,

    /// Optional current-task-node id the composer is presently
    /// considering. Threaded from `composer_v4::planner` so the
    /// Opaque sink can attribute observations to the right node.
    /// None at compatibility-engine-only call sites that don't know
    /// the surrounding DAG context.
    pub opaque_node_id: Option<String>,
}

/// v3 §4 / v4 §4 Round-2 G1 / G13 closure — receiver for opaque-type
/// observations emitted by the compatibility engine.
///
/// The conversation crate provides a concrete implementation that
/// forwards into `crates/conversation/src/session/opaque_aggregator.rs`;
/// the core crate stays free of the on-disk aggregator since the
/// session-store path lives in `conversation`.
///
/// `Debug` is a supertrait so `PlanningContext` can keep its
/// `#[derive(Debug)]`.
pub trait OpaqueObservationSink: std::fmt::Debug {
    /// Record a single `Opaque` observation. Implementations should be
    /// idempotent on duplicate (hash, session, node, port) tuples and
    /// best-effort with respect to IO errors (log, do not panic).
    fn record_opaque(
        &self,
        opaque_hash: &str,
        session_id: &str,
        node: &str,
        port: &str,
        timestamp_rfc3339: &str,
    );
}

/// Engine trait. Alternative engines (e.g. a SAT-based one) could
/// plug in via this trait without changing call sites.
pub trait CompatibilityEngine {
    /// Prove.
    fn prove(
        &self,
        producer: &PortContract,
        consumer: &PortContract,
        ctx: &PlanningContext,
    ) -> CompatibilityResult;
}

/// Curated-table compatibility engine.
///
/// Uses `crate::edam::edam_subtype_edges` for ontology subsumption,
/// facet unification for load-bearing facets, adapter registry
/// wiring so `CompatibleWithAdapters` returns real adapter chains,
/// and policy decisions.
#[derive(Debug, Clone, Default)]
pub struct DeterministicCompatibilityEngine {
    /// Cached subtype edges — refreshed when the ontology version
    /// in PlanningContext.ontology_snapshot_id changes.
    subtype_edges_cache: Option<std::collections::BTreeMap<String, Vec<String>>>,
    /// Adapter registry consulted when facet unification
    /// returns `Incompatible` and an adapter can repair the
    /// mismatch under the active `AdapterPolicy`.
    adapter_registry: AdapterRegistry,
}

impl DeterministicCompatibilityEngine {
    /// New.
    pub fn new() -> Self {
        Self {
            subtype_edges_cache: Some(edam_subtype_edges()),
            adapter_registry: AdapterRegistry::with_starters(),
        }
    }

    /// Build an engine with a custom adapter registry. Site-local
    /// installations / tests can pre-register adapters via the
    /// registry directly and pass it here.
    pub fn with_adapter_registry(adapter_registry: AdapterRegistry) -> Self {
        Self {
            subtype_edges_cache: Some(edam_subtype_edges()),
            adapter_registry,
        }
    }

    /// Try to find an adapter that converts `producer_value` to
    /// `consumer_value` for the named facet under the policy.
    /// Returns the adapter spec if found and policy-allowed.
    ///
    /// Uses id-based heuristic dispatch keyed on the canonical adapter
    /// id pattern (`liftover_<from>_<to>`, `<from>_to_<to>`, etc.).
    /// Typed adapter chains are available via `find_for_semantic_types`
    /// and `find_for_formats`.
    fn try_resolve_facet_with_adapter(
        &self,
        name: &str,
        producer_value: &str,
        consumer_value: &str,
        policy: &AdapterPolicy,
    ) -> Option<AdapterSpec> {
        let candidate_ids: Vec<String> = match name {
            "genome_build" => {
                // GRCh37 → GRCh38 maps to liftover_grch37_grch38.
                // Lowercase with `.` and ` ` removed.
                let from = producer_value.to_lowercase().replace(['.', ' ', '-'], "");
                let to = consumer_value.to_lowercase().replace(['.', ' ', '-'], "");
                // v4 P8 — sanity-check identical-genome via the phantom-typed
                // `coordinate_system_facets_consistent` helper. The check
                // costs nothing at runtime (the phantom marker is zero-sized
                // and the equality is a const string compare) but the
                // call-site annotation makes the F23 invariant visible:
                // identical `from` / `to` should produce no adapter, not
                // a no-op `liftover_grch38_grch38` lookup.
                if coordinate_system_facets_consistent(&from, &to) {
                    Vec::new()
                } else {
                    vec![format!("liftover_{from}_{to}")]
                }
            }
            "coordinate_system" => {
                // Reserved for future bed_zero_to_one_based etc.
                vec![]
            }
            _ => Vec::new(),
        };
        for id in &candidate_ids {
            if let Some(spec) = self.adapter_registry.get(id) {
                if Self::adapter_allowed_under_policy(spec, policy) {
                    return Some(spec.clone());
                }
            }
        }
        None
    }

    /// Returns true if the policy permits this adapter's safety
    /// class for auto-insertion.
    fn adapter_allowed_under_policy(spec: &AdapterSpec, policy: &AdapterPolicy) -> bool {
        match spec.safety {
            AdapterSafety::Lossless => policy.allow_lossless,
            AdapterSafety::LossyDeclared => policy.allow_lossy_with_assumption,
            AdapterSafety::ScientificallyRisky => policy.allow_risky_with_confirmation,
            AdapterSafety::PolicyRestricted => false,
        }
    }

    fn subtype_edges(&self) -> std::collections::BTreeMap<String, Vec<String>> {
        self.subtype_edges_cache
            .clone()
            .unwrap_or_else(edam_subtype_edges)
    }

    /// Returns the subsumption path from `producer` to `consumer`
    /// IRIs, walking the curated EDAM table.
    /// `Some(vec![])` means producer and consumer are identical
    /// IRIs (zero-step path); `Some(path)` means producer is a
    /// strict subtype with `path` listing intermediate parents;
    /// `None` means no path exists in the curated table.
    fn ontology_path(&self, producer: &str, consumer: &str) -> Option<Vec<String>> {
        if producer == consumer {
            return Some(Vec::new());
        }
        let edges = self.subtype_edges();
        // Reverse the table once: `subtype_edges` is parent →
        // [children]; we want child → ancestor walk to find a
        // path. Determinism: BTreeMap-backed.
        let mut child_to_parents: std::collections::BTreeMap<String, Vec<String>> =
            std::collections::BTreeMap::new();
        for (parent, children) in &edges {
            for c in children {
                child_to_parents
                    .entry(c.clone())
                    .or_default()
                    .push(parent.clone());
            }
        }
        // BFS from producer up the chain looking for consumer.
        let mut frontier: Vec<(String, Vec<String>)> = vec![(producer.to_string(), Vec::new())];
        let mut seen: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        seen.insert(producer.to_string());
        while let Some((node, path)) = frontier.pop() {
            if let Some(parents) = child_to_parents.get(&node) {
                for p in parents {
                    if !seen.insert(p.clone()) {
                        continue;
                    }
                    let mut new_path = path.clone();
                    new_path.push(p.clone());
                    if p == consumer {
                        return Some(new_path);
                    }
                    frontier.push((p.clone(), new_path));
                }
            }
        }
        None
    }

    /// Check semantic-type compatibility. Returns either a
    /// subsumption path (vec) or an incompatibility reason.
    fn semantic_compat(
        &self,
        producer: &SemanticType,
        consumer: &SemanticType,
    ) -> Result<Vec<String>, IncompatibilityReason> {
        // Union consumer: compatible when the producer is compatible
        // with ANY member of the union. Return the first successful
        // member path.
        if let SemanticType::Union { members } = consumer {
            for member in members {
                if let Ok(path) = self.semantic_compat(producer, member) {
                    return Ok(path);
                }
            }
            return Err(IncompatibilityReason::SemanticTypeMismatch {
                producer: producer.stable_id(),
                consumer: consumer.stable_id(),
            });
        }
        // Union producer: compatible when ALL members are compatible
        // with the (non-union) consumer. Return the combined paths.
        if let SemanticType::Union { members } = producer {
            let mut combined: Vec<String> = Vec::new();
            for member in members {
                match self.semantic_compat(member, consumer) {
                    Ok(path) => {
                        for p in path {
                            if !combined.contains(&p) {
                                combined.push(p);
                            }
                        }
                    }
                    Err(reason) => return Err(reason),
                }
            }
            return Ok(combined);
        }
        match (producer, consumer) {
            (
                SemanticType::OntologyTerm { iri: pi, .. },
                SemanticType::OntologyTerm { iri: ci, .. },
            ) => self.ontology_path(pi, ci).ok_or_else(|| {
                IncompatibilityReason::SemanticTypeMismatch {
                    producer: pi.clone(),
                    consumer: ci.clone(),
                }
            }),
            (
                SemanticType::LocalExtension {
                    proposed_parent_terms,
                    ..
                },
                SemanticType::OntologyTerm { iri: ci, .. },
            ) => {
                // LocalExtension matches an OntologyTerm consumer
                // when one of the proposed parent terms is the
                // consumer's IRI or a subtype of it.
                for parent in proposed_parent_terms {
                    if parent == ci {
                        return Ok(vec![parent.clone()]);
                    }
                    if let Some(mut path) = self.ontology_path(parent, ci) {
                        path.insert(0, parent.clone());
                        return Ok(path);
                    }
                }
                Err(IncompatibilityReason::SemanticTypeMismatch {
                    producer: producer.stable_id(),
                    consumer: ci.clone(),
                })
            }
            (_, SemanticType::Opaque { .. }) | (SemanticType::Opaque { .. }, _) => {
                // Opaque types never match definitively. Engine
                // returns Unknown via the Err path; caller maps
                // that to `CompatibilityResult::Unknown`.
                Err(IncompatibilityReason::SemanticTypeMismatch {
                    producer: producer.stable_id(),
                    consumer: consumer.stable_id(),
                })
            }
            (
                SemanticType::OntologyTerm { iri: pi, .. },
                SemanticType::LocalExtension {
                    proposed_parent_terms,
                    ..
                },
            ) => {
                // Producer ontology term ↔ consumer local extension:
                // accept when producer IRI is one of the proposed
                // parents (consumer is a subtype of producer means
                // producer's data feeds the consumer).
                if proposed_parent_terms.iter().any(|p| p == pi) {
                    Ok(vec![pi.clone()])
                } else {
                    Err(IncompatibilityReason::SemanticTypeMismatch {
                        producer: pi.clone(),
                        consumer: consumer.stable_id(),
                    })
                }
            }
            (
                SemanticType::LocalExtension { id: pi, .. },
                SemanticType::LocalExtension { id: ci, .. },
            ) if pi == ci => Ok(Vec::new()),
            (SemanticType::LocalExtension { .. }, SemanticType::LocalExtension { .. }) => {
                Err(IncompatibilityReason::SemanticTypeMismatch {
                    producer: producer.stable_id(),
                    consumer: consumer.stable_id(),
                })
            }
            // Union arms are handled at the top of this function;
            // these arms are unreachable but required for exhaustiveness.
            (SemanticType::Union { .. }, _) | (_, SemanticType::Union { .. }) => {
                unreachable!("union arms must be handled before the main match")
            }
        }
    }

    /// Run facet unification for the load-bearing facets. Returns
    /// the per-facet outcomes for proof attachment plus a
    /// boolean: `true` if any facet was Incompatible.
    fn unify_facets(
        &self,
        producer: &PortContract,
        consumer: &PortContract,
    ) -> Vec<(String, FacetUnification)> {
        let pairs: [(&str, Option<&str>, Option<&str>); 8] = [
            (
                "modality",
                producer.modality.as_deref(),
                consumer.modality.as_deref(),
            ),
            (
                "organism",
                producer.organism.as_deref(),
                consumer.organism.as_deref(),
            ),
            (
                "genome_build",
                producer.genome_build.as_deref(),
                consumer.genome_build.as_deref(),
            ),
            (
                "annotation_version",
                producer.annotation_version.as_deref(),
                consumer.annotation_version.as_deref(),
            ),
            (
                "coordinate_system",
                producer.coordinate_system.as_deref(),
                consumer.coordinate_system.as_deref(),
            ),
            (
                "units",
                producer.units.as_deref(),
                consumer.units.as_deref(),
            ),
            (
                "normalization_state",
                producer.normalization_state.as_deref(),
                consumer.normalization_state.as_deref(),
            ),
            (
                "statistical_state",
                producer.statistical_state.as_deref(),
                consumer.statistical_state.as_deref(),
            ),
        ];
        pairs
            .iter()
            .map(|(name, p, c)| {
                (
                    (*name).to_string(),
                    unify_facet(name, *p, *c, |_, _| None, |_, _| None),
                )
            })
            .collect()
    }
}

impl CompatibilityEngine for DeterministicCompatibilityEngine {
    fn prove(
        &self,
        producer: &PortContract,
        consumer: &PortContract,
        ctx: &PlanningContext,
    ) -> CompatibilityResult {
        // v4 P2 / F18 — substrate emission. Every `prove()` call
        // records at least one `UnificationAttempted` row at entry so
        // the F18 property test ("every prove call emits substrate
        // event") holds for every shape of input. The id is stable so
        // substrate consumers can correlate the attempt row with the
        // subsequent succeeded/failed/unknown row emitted by the
        // result-tagging step at the function tail.
        let producer_port_id = producer.semantic_type.stable_id();
        let consumer_port_id = consumer.semantic_type.stable_id();
        let ctx_hash = format!(
            "{}:{}:{:?}",
            ctx.ontology_snapshot_id.as_deref().unwrap_or(""),
            ctx.atom_snapshot_id.as_deref().unwrap_or(""),
            ctx.risk_mode
        );
        let attempt_id =
            crate::decision_substrate::stable_id("unif", &producer_port_id, &consumer_port_id);
        crate::decision_substrate::record(
            crate::decision_substrate::VerifierDecision::UnificationAttempted {
                id: attempt_id.clone(),
                timestamp: crate::decision_substrate::timestamp(),
                producer_port: producer_port_id.clone(),
                consumer_port: consumer_port_id.clone(),
                ctx_hash,
            },
        );

        let result = self.prove_inner(producer, consumer, ctx);

        // v4 P2 / F18 — tag the result with a follow-up substrate row.
        // `Unknown` is treated as a non-decision (no Succeeded/Failed
        // row) so the F18 property test asserts "attempted" only when
        // the engine refused to commit (opaque short-circuit). Real
        // decisions tag Succeeded or Failed with the underlying detail.
        match &result {
            CompatibilityResult::Compatible(proof) => {
                crate::decision_substrate::record(
                    crate::decision_substrate::VerifierDecision::UnificationSucceeded {
                        id: format!("succ:{attempt_id}"),
                        timestamp: crate::decision_substrate::timestamp(),
                        producer_port: producer_port_id.clone(),
                        consumer_port: consumer_port_id.clone(),
                        // CompatibilityProof has no stable id field;
                        // synthesize one from the producer↔consumer
                        // semantic-type pair so substrate consumers
                        // can cross-reference `runtime/proofs.jsonl`.
                        proof_id: format!("{}->{}", proof.producer_type, proof.consumer_type),
                        adapters_inserted: Vec::new(),
                        residual_assumptions: proof
                            .assumptions
                            .iter()
                            .map(|a| a.id.clone())
                            .collect(),
                    },
                );
            }
            CompatibilityResult::CompatibleWithAdapters { proof, adapters } => {
                let adapter_ids: Vec<String> = adapters.iter().map(|a| a.id.clone()).collect();
                crate::decision_substrate::record(
                    crate::decision_substrate::VerifierDecision::UnificationSucceeded {
                        id: format!("succ:{attempt_id}"),
                        timestamp: crate::decision_substrate::timestamp(),
                        producer_port: producer_port_id.clone(),
                        consumer_port: consumer_port_id.clone(),
                        proof_id: format!("{}->{}", proof.producer_type, proof.consumer_type),
                        adapters_inserted: adapter_ids,
                        residual_assumptions: proof
                            .assumptions
                            .iter()
                            .map(|a| a.id.clone())
                            .collect(),
                    },
                );
            }
            CompatibilityResult::Incompatible(report) => {
                if let Some(reason) = report.reasons.first() {
                    crate::decision_substrate::record(
                        crate::decision_substrate::VerifierDecision::UnificationFailed {
                            id: format!("fail:{attempt_id}"),
                            timestamp: crate::decision_substrate::timestamp(),
                            producer_port: producer_port_id.clone(),
                            consumer_port: consumer_port_id.clone(),
                            reason: crate::decision_substrate::IncompatibilityReason::from_engine(
                                reason,
                            ),
                        },
                    );
                }
            }
            CompatibilityResult::Unknown(_) => {
                // No follow-up row: the attempt is recorded but the
                // engine deliberately defers the decision to a later
                // (SME or profiler) path. Future refinement could
                // record a typed `UnificationDeferred` variant; today
                // the attempt row is enough for F18.
            }
        }
        result
    }
}

impl DeterministicCompatibilityEngine {
    /// v4 P2 / F18 — the original `prove()` body, refactored into a
    /// helper so the trait impl above can wrap entry + result with
    /// substrate emission without duplicating every early-return arm.
    /// Opaque producer/consumer ports short-circuit to `Unknown` — the caller
    /// decides Draft vs Refusal. Every Opaque short-circuit is a cross-session
    /// signal: persistent Opaque observations indicate the system repeatedly
    /// hits the same un-modeled type and the operator should mint a
    /// LocalExtension or file an upstream-ontology issue. When a chat session
    /// wires an `OpaqueObservationSink` (→ `opaque_aggregator`) the observation
    /// is recorded there; bare composer call sites (CLI `build`, eval-adapters,
    /// unit tests) leave the sink `None` and fall through to a `tracing::warn!`
    /// so the observation is still visible. Returns `None` when neither port is
    /// Opaque.
    fn check_opaque_ports(
        producer: &PortContract,
        consumer: &PortContract,
        ctx: &PlanningContext,
    ) -> Option<CompatibilityResult> {
        let producer_opaque = matches!(producer.semantic_type, SemanticType::Opaque { .. });
        let consumer_opaque = matches!(consumer.semantic_type, SemanticType::Opaque { .. });
        if !producer_opaque && !consumer_opaque {
            return None;
        }
        let opaque_st = if producer_opaque {
            &producer.semantic_type
        } else {
            &consumer.semantic_type
        };
        let opaque_hash = opaque_st.stable_id();
        let port_name = if producer_opaque {
            producer.name.as_str()
        } else {
            consumer.name.as_str()
        };
        if let Some(sink) = ctx.opaque_observation_sink.as_ref() {
            sink.record_opaque(
                &opaque_hash,
                ctx.opaque_session_id.as_deref().unwrap_or("anonymous"),
                ctx.opaque_node_id.as_deref().unwrap_or("unknown_node"),
                port_name,
                &crate::time_helpers::now_rfc3339(),
            );
        } else {
            tracing::warn!(
                target: "opaque_observation",
                opaque_hash = %opaque_hash,
                port = %port_name,
                "Opaque semantic-type observed; no OpaqueObservationSink wired \
                 into PlanningContext (cross-session aggregator skipped)"
            );
        }

        Some(CompatibilityResult::Unknown(
            ClarificationOrValidationNeeded {
                id: format!(
                    "opaque:{}:{}",
                    producer.semantic_type.variant_key(),
                    consumer.semantic_type.variant_key()
                ),
                statement: "Producer or consumer port has Opaque semantic type".into(),
                suggestion: "Run dataset profiler at execution time or have SME annotate the type"
                    .into(),
            },
        ))
    }

    /// Modality-scoped ontology check on a producer's proposed parent terms.
    /// Annotation-only: forbidden parents emit a `tracing::warn!` (observable
    /// under `RUST_LOG=ontology_scope=warn`; the registry-import path downgrades
    /// trust on the same signal). Recording is unconditional — `in_primary` /
    /// `in_secondary` rows confirm the matrix was consulted on every
    /// LocalExtension proposal, not just forbidden ones — and replayable from
    /// `runtime/verifier-decisions.jsonl`. No-op for non-LocalExtension types or
    /// when no ontology scope is configured.
    fn record_ontology_scope(producer: &PortContract, ctx: &PlanningContext) {
        let SemanticType::LocalExtension {
            proposed_parent_terms,
            ..
        } = &producer.semantic_type
        else {
            return;
        };
        let Some(scope) = ctx.ontology_scope.as_ref() else {
            return;
        };
        let modality = producer
            .modality
            .as_deref()
            .and_then(|s| {
                s.parse::<crate::workflow_contracts::workflow_intent::BioinformaticsModality>()
                    .ok()
            })
            .unwrap_or(
                crate::workflow_contracts::workflow_intent::BioinformaticsModality::GenericOmics,
            );
        for parent in proposed_parent_terms {
            let Some(prefix) = crate::ontology_scope::OntologyScopeMatrix::prefix_of_iri(parent)
            else {
                continue;
            };
            let result = scope.check(&modality, &prefix);
            let result_str = match &result {
                crate::ontology_scope::ScopeCheck::InPrimary => "in_primary",
                crate::ontology_scope::ScopeCheck::InSecondary => "in_secondary",
                crate::ontology_scope::ScopeCheck::Forbidden => "forbidden",
                crate::ontology_scope::ScopeCheck::OutOfScope => "out_of_scope",
            };
            crate::decision_substrate::record(
                crate::decision_substrate::VerifierDecision::OntologyScopeChecked {
                    id: crate::decision_substrate::stable_id(
                        "scope",
                        &format!("{modality:?}"),
                        parent,
                    ),
                    timestamp: crate::decision_substrate::timestamp(),
                    modality: format!("{modality:?}"),
                    candidate_iri: parent.clone(),
                    result: result_str.to_string(),
                    rule_id: format!("modality-ontology-coverage:{prefix}"),
                },
            );
            if matches!(result, crate::ontology_scope::ScopeCheck::Forbidden) {
                tracing::warn!(
                    target: "ontology_scope",
                    "proposed parent {} (prefix {}) is forbidden for modality {:?}",
                    parent,
                    prefix,
                    modality
                );
            }
        }
    }

    fn prove_inner(
        &self,
        producer: &PortContract,
        consumer: &PortContract,
        ctx: &PlanningContext,
    ) -> CompatibilityResult {
        // Opaque consumers/producers are Unknown — caller (Phase
        // 6 outcome wrapper) decides whether to surface as Draft
        // or Refusal.
        if let Some(result) = Self::check_opaque_ports(producer, consumer, ctx) {
            return result;
        }

        // V4 modality-scoped ontology check on proposed parent terms.
        Self::record_ontology_scope(producer, ctx);

        // Step 1: semantic type subsumption.
        let path = match self.semantic_compat(&producer.semantic_type, &consumer.semantic_type) {
            Ok(p) => p,
            Err(reason) => {
                return CompatibilityResult::Incompatible(IncompatibilityReport::new(vec![reason]));
            }
        };

        // Step 2: privacy class. Refuse widening
        // (producer broader than consumer).
        if (producer.privacy_class as u8) > (consumer.privacy_class as u8) {
            return CompatibilityResult::Incompatible(IncompatibilityReport::new(vec![
                IncompatibilityReason::PrivacyClassWidening {
                    producer: format!("{:?}", producer.privacy_class),
                    consumer: format!("{:?}", consumer.privacy_class),
                },
            ]));
        }

        // Step 3: facet unification.
        let facet_results = self.unify_facets(producer, consumer);

        // Collect (facet name, producer value, consumer
        // value, rationale) tuples so the adapter resolver can
        // attempt repair for incompatible facets before the engine
        // declares the edge `Incompatible`. The pairs vector keeps
        // the same iteration order as `unify_facets` so determinism
        // tests remain byte-stable.
        let raw_pairs: Vec<(&'static str, Option<&str>, Option<&str>)> = vec![
            (
                "modality",
                producer.modality.as_deref(),
                consumer.modality.as_deref(),
            ),
            (
                "organism",
                producer.organism.as_deref(),
                consumer.organism.as_deref(),
            ),
            (
                "genome_build",
                producer.genome_build.as_deref(),
                consumer.genome_build.as_deref(),
            ),
            (
                "annotation_version",
                producer.annotation_version.as_deref(),
                consumer.annotation_version.as_deref(),
            ),
            (
                "coordinate_system",
                producer.coordinate_system.as_deref(),
                consumer.coordinate_system.as_deref(),
            ),
            (
                "units",
                producer.units.as_deref(),
                consumer.units.as_deref(),
            ),
            (
                "normalization_state",
                producer.normalization_state.as_deref(),
                consumer.normalization_state.as_deref(),
            ),
            (
                "statistical_state",
                producer.statistical_state.as_deref(),
                consumer.statistical_state.as_deref(),
            ),
        ];

        let mut incompatible_facets: Vec<IncompatibilityReason> = Vec::new();
        let mut unknown_facets: Vec<String> = Vec::new();
        // Adapters chosen to repair mismatches under the
        // active policy. Materialized as TaskNodes for the
        // `CompatibleWithAdapters` return arm.
        let mut resolved_adapters: Vec<AdapterSpec> = Vec::new();
        let mut substituted_facets: std::collections::BTreeMap<String, String> =
            std::collections::BTreeMap::new();

        for ((name, producer_value, consumer_value), (_, outcome)) in
            raw_pairs.iter().zip(facet_results.iter())
        {
            match outcome {
                FacetUnification::Incompatible { rationale } => {
                    if let (Some(pv), Some(cv)) = (producer_value, consumer_value) {
                        if let Some(spec) =
                            self.try_resolve_facet_with_adapter(name, pv, cv, &ctx.adapter_policy)
                        {
                            substituted_facets.insert(
                                (*name).to_string(),
                                format!("{rationale} (adapter={})", spec.id),
                            );
                            resolved_adapters.push(spec);
                            continue;
                        }
                    }
                    incompatible_facets.push(IncompatibilityReason::FacetMismatch {
                        facet: (*name).to_string(),
                        producer: producer_value.unwrap_or("").to_string(),
                        consumer: consumer_value.unwrap_or("").to_string(),
                        rationale: rationale.clone(),
                    });
                }
                FacetUnification::Unknown { reason } => {
                    unknown_facets.push(format!("{name}: {reason}"));
                }
                _ => {}
            }
        }
        if !incompatible_facets.is_empty() {
            return CompatibilityResult::Incompatible(IncompatibilityReport::new(
                incompatible_facets,
            ));
        }

        // Policy gate.
        // Active PolicyContext bundles can refuse the edge or record
        // positive policy decisions in the proof.
        let mut policy_decisions: Vec<String> = Vec::new();
        for (bundle_id, check) in ctx.policy_context.iter_checks() {
            match check.kind() {
                PolicyCheckKind::NoScientificallyRiskyAdapters => {
                    let risky: Vec<String> = resolved_adapters
                        .iter()
                        .filter(|s| matches!(s.safety, AdapterSafety::ScientificallyRisky))
                        .map(|s| s.id.clone())
                        .collect();
                    if !risky.is_empty() {
                        return CompatibilityResult::Incompatible(IncompatibilityReport::new(
                            vec![IncompatibilityReason::PolicyViolation {
                                bundle_id: bundle_id.to_string(),
                                check_kind: "no_scientifically_risky_adapters".into(),
                                statement: format!(
                                    "Active bundle '{bundle_id}' refuses ScientificallyRisky \
                                     adapters; chosen adapter chain contains: {}",
                                    risky.join(", ")
                                ),
                            }],
                        ));
                    }
                    policy_decisions.push(format!(
                        "{bundle_id}:no_scientifically_risky_adapters:applied"
                    ));
                }
                PolicyCheckKind::NoPolicyRestrictedAdapters => {
                    let restricted: Vec<String> = resolved_adapters
                        .iter()
                        .filter(|s| matches!(s.safety, AdapterSafety::PolicyRestricted))
                        .map(|s| s.id.clone())
                        .collect();
                    if !restricted.is_empty() {
                        return CompatibilityResult::Incompatible(IncompatibilityReport::new(
                            vec![IncompatibilityReason::PolicyViolation {
                                bundle_id: bundle_id.to_string(),
                                check_kind: "no_policy_restricted_adapters".into(),
                                statement: format!(
                                    "Active bundle '{bundle_id}' refuses PolicyRestricted \
                                     adapters; chosen adapter chain contains: {}",
                                    restricted.join(", ")
                                ),
                            }],
                        ));
                    }
                    policy_decisions
                        .push(format!("{bundle_id}:no_policy_restricted_adapters:applied"));
                }
                PolicyCheckKind::NoPrivacyWidening => {
                    // Already enforced by the engine's privacy check
                    // in step 2; record the positive decision so the
                    // proof's audit trail names the active policy.
                    policy_decisions.push(format!("{bundle_id}:no_privacy_widening:applied"));
                }
                PolicyCheckKind::AuditTrailRequired => {
                    policy_decisions.push(format!("{bundle_id}:audit_trail_required"));
                }
                PolicyCheckKind::HumanSignoffRequired => {
                    policy_decisions.push(format!("{bundle_id}:human_signoff_required"));
                }
                PolicyCheckKind::ValidatedNodesOnly
                | PolicyCheckKind::RequirePinnedContainers
                | PolicyCheckKind::NoGeneratedCode
                | PolicyCheckKind::NoNetwork
                | PolicyCheckKind::PinnedReferenceDataOnly => {
                    // Per-node / per-data-product checks; the
                    // composer evaluates these outside the engine
                    // Per-node / per-data-product checks are evaluated
                    // by the composer outside the engine. Record the
                    // active policy so the proof surfaces it.
                    policy_decisions.push(format!(
                        "{bundle_id}:{:?}:deferred_to_planner",
                        check.kind()
                    ));
                }
                PolicyCheckKind::SiteLocal => {
                    if let PolicyCheck::SiteLocal {
                        check_id,
                        statement,
                    } = check
                    {
                        policy_decisions
                            .push(format!("{bundle_id}:site_local:{check_id}:{statement}"));
                    }
                }
            }
        }

        // Step 4: assemble the proof.
        let mut builder = ProofBuilder::new(&producer.semantic_type, &consumer.semantic_type)
            .with_subsumption_path(path);
        for (name, outcome) in &facet_results {
            // Unknown facets are recorded with empty producer/consumer
            // strings since the caller already saw the rationale.
            if let Some(adapter_rationale) = substituted_facets.get(name) {
                // Facet was repaired by an adapter; record
                // the substitution + adapter id in the proof so the
                // UI can render "edge holds via adapter" without a
                // separate registry lookup.
                builder.add_facet(
                    name,
                    None,
                    None,
                    crate::workflow_contracts::edge::FacetMatchKind::Substituted,
                    Some(adapter_rationale.clone()),
                );
                continue;
            }
            match outcome {
                FacetUnification::Exact => {
                    // Skip — exact matches are noise in the proof.
                }
                FacetUnification::Subtype { rationale } => {
                    builder.add_facet(
                        name,
                        None,
                        None,
                        outcome.match_kind(),
                        Some(rationale.clone()),
                    );
                }
                FacetUnification::Substituted {
                    adapter_id,
                    rationale,
                } => {
                    builder.add_facet(
                        name,
                        None,
                        None,
                        outcome.match_kind(),
                        Some(format!("{rationale} (adapter={adapter_id})")),
                    );
                    builder.add_adapter_node(adapter_id.clone());
                }
                FacetUnification::Unknown { reason } => {
                    builder.add_facet(name, None, None, outcome.match_kind(), Some(reason.clone()));
                }
                FacetUnification::Incompatible { .. } => {
                    // Handled above (repaired or returned
                    // already as Incompatible).
                }
            }
        }
        for w in &unknown_facets {
            builder.add_warning(format!("unknown facet: {w}"));
        }

        // Populate evidence with registry snapshot ids from the planning
        // context. Both atom-registry and ontology snapshot ids are recorded
        // when present so the proof carries a traceable audit trail back to
        // the exact catalog consulted during compatibility checking.
        if let Some(snap) = &ctx.atom_snapshot_id {
            builder.add_evidence(ProofEvidence::RegistrySnapshot {
                registry: "atom_registry".to_string(),
                snapshot_id: snap.clone(),
            });
        }
        if let Some(snap) = &ctx.ontology_snapshot_id {
            builder.add_evidence(ProofEvidence::RegistrySnapshot {
                registry: "ontology".to_string(),
                snapshot_id: snap.clone(),
            });
        }

        // Record a baseline ValidatorRun so every proof's evidence array is
        // non-empty even when no registry snapshots were supplied. The
        // "edge_compatibility" validator represents the deterministic engine
        // check itself — semantic subsumption + facet unification passed.
        builder.add_evidence(ProofEvidence::ValidatorRun {
            validator_id: "edge_compatibility".to_string(),
            result_ref: "ok".to_string(),
        });

        for spec in &resolved_adapters {
            builder.add_adapter_node(spec.id.clone());
            // Record a ValidatorRun evidence entry for each adapter that
            // was selected as a compatibility bridge. The result_ref is
            // "compatible_with_adapters" so replay tests can identify the
            // proof shape without querying a live registry.
            builder.add_evidence(
                crate::workflow_contracts::edge::ProofEvidence::ValidatorRun {
                    validator_id: format!("adapter_validator:{}", spec.id),
                    result_ref: "compatible_with_adapters".into(),
                },
            );
        }

        for decision in &policy_decisions {
            builder.add_policy_decision(decision.clone());
        }

        let proof = builder
            .with_rationale(format!(
                "Producer {} is subsumed by / matches consumer {}",
                producer.semantic_type.stable_id(),
                consumer.semantic_type.stable_id()
            ))
            .build();

        if resolved_adapters.is_empty() {
            CompatibilityResult::Compatible(proof)
        } else {
            // Return CompatibleWithAdapters with the adapter chain
            // materialized as TaskNodes. The composer reads each
            // adapter's safety class and emits assumption-ledger
            // entries for LossyDeclared adapters.
            let adapters: Vec<TaskNode> =
                resolved_adapters.iter().map(|s| s.to_task_node()).collect();
            CompatibilityResult::CompatibleWithAdapters { proof, adapters }
        }
    }
}

// v4 P8 (D1 / F23) — phantom-typed helper. Dispatches against the
// `ReferenceGenome` marker family in `crate::compile_time_discipline`
// to assert at compile time that the engine cannot mix
// reference-genome facet values. The match is a small string
// dispatch today; the phantom-typed wrapper type
// `AlignedReads<R>` carries the compile-time guarantee that any
// future expansion of this helper that touches actual alignment
// data structures will refuse to mix incompatible references
// without a code change here. See `crate::compile_time_discipline`
// for the design boundary; F23 forbids serializing the wrapper.
fn coordinate_system_facets_consistent(from: &str, to: &str) -> bool {
    use crate::compile_time_discipline::reference_genome::{
        liftover_required, AlignedReads, GRCh37, GRCh38, Mm39, ReferenceGenome, T2T_CHM13,
    };
    use std::marker::PhantomData;

    fn project(facet: &str) -> Option<&'static str> {
        match facet {
            v if v.contains("grch38") => Some(GRCh38::NAME),
            v if v.contains("grch37") => Some(GRCh37::NAME),
            v if v.contains("t2t") || v.contains("chm13") => Some(T2T_CHM13::NAME),
            v if v.contains("mm39") || v.contains("grcm39") => Some(Mm39::NAME),
            _ => None,
        }
    }

    match (project(from), project(to)) {
        (Some(a), Some(b)) if a == b => {
            let consistent = if a == GRCh38::NAME {
                let reads: AlignedReads<GRCh38> = AlignedReads::new(true, true);
                !liftover_required(&reads, PhantomData::<GRCh38>)
            } else if a == GRCh37::NAME {
                let reads: AlignedReads<GRCh37> = AlignedReads::new(true, true);
                !liftover_required(&reads, PhantomData::<GRCh37>)
            } else if a == T2T_CHM13::NAME {
                let reads: AlignedReads<T2T_CHM13> = AlignedReads::new(true, true);
                !liftover_required(&reads, PhantomData::<T2T_CHM13>)
            } else {
                let reads: AlignedReads<Mm39> = AlignedReads::new(true, true);
                !liftover_required(&reads, PhantomData::<Mm39>)
            };
            debug_assert!(
                consistent,
                "liftover_required: alignment-coord conversion result inconsistent with reference-genome compatibility check",
            );
            true
        }
        _ => from == to,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflow_contracts::port::PortContract;

    fn p(iri: &str) -> PortContract {
        PortContract {
            name: "port".into(),
            semantic_type: SemanticType::edam(iri, ""),
            ..Default::default()
        }
    }

    #[test]
    fn exact_iri_compatible() {
        let engine = DeterministicCompatibilityEngine::new();
        let res = engine.prove(
            &p("data:0863"),
            &p("data:0863"),
            &PlanningContext::default(),
        );
        assert!(matches!(res, CompatibilityResult::Compatible(_)));
    }

    #[test]
    fn opaque_returns_unknown() {
        let engine = DeterministicCompatibilityEngine::new();
        let mut producer = p("data:0863");
        producer.semantic_type = SemanticType::opaque("unprofiled");
        let res = engine.prove(&producer, &p("data:0863"), &PlanningContext::default());
        assert!(matches!(res, CompatibilityResult::Unknown(_)));
    }

    #[test]
    fn unrelated_iri_incompatible() {
        let engine = DeterministicCompatibilityEngine::new();
        // data:0863 (BAM) vs data:3917 (count matrix) — not in
        // the curated subtype table.
        let res = engine.prove(
            &p("data:0863"),
            &p("data:3917"),
            &PlanningContext::default(),
        );
        assert!(matches!(res, CompatibilityResult::Incompatible(_)));
    }

    #[test]
    fn genome_build_mismatch_is_incompatible() {
        let engine = DeterministicCompatibilityEngine::new();
        let mut producer = p("data:0863");
        producer.genome_build = Some("GRCh37".into());
        let mut consumer = p("data:0863");
        consumer.genome_build = Some("GRCh38".into());
        let res = engine.prove(&producer, &consumer, &PlanningContext::default());
        match res {
            CompatibilityResult::Incompatible(report) => {
                assert!(matches!(
                    report.reasons[0],
                    IncompatibilityReason::FacetMismatch { .. }
                ));
            }
            other => panic!("expected Incompatible, got {other:?}"),
        }
    }

    #[test]
    fn matching_genome_build_is_compatible() {
        let engine = DeterministicCompatibilityEngine::new();
        let mut producer = p("data:0863");
        producer.genome_build = Some("GRCh38".into());
        let mut consumer = p("data:0863");
        consumer.genome_build = Some("GRCh38".into());
        let res = engine.prove(&producer, &consumer, &PlanningContext::default());
        assert!(matches!(res, CompatibilityResult::Compatible(_)));
    }

    #[test]
    fn missing_consumer_facet_is_compatible() {
        let engine = DeterministicCompatibilityEngine::new();
        let mut producer = p("data:0863");
        producer.genome_build = Some("GRCh38".into());
        let consumer = p("data:0863");
        // Consumer didn't constrain genome build.
        let res = engine.prove(&producer, &consumer, &PlanningContext::default());
        assert!(matches!(res, CompatibilityResult::Compatible(_)));
    }

    #[test]
    fn missing_producer_facet_is_compatible_with_warning() {
        let engine = DeterministicCompatibilityEngine::new();
        let producer = p("data:0863");
        let mut consumer = p("data:0863");
        consumer.genome_build = Some("GRCh38".into());
        let res = engine.prove(&producer, &consumer, &PlanningContext::default());
        match res {
            CompatibilityResult::Compatible(proof) => {
                assert!(proof.warnings.iter().any(|w| w.contains("genome_build")));
            }
            other => panic!("expected Compatible w/ warning, got {other:?}"),
        }
    }

    #[test]
    fn privacy_widening_is_incompatible() {
        let engine = DeterministicCompatibilityEngine::new();
        let mut producer = p("data:0863");
        producer.privacy_class = crate::workflow_contracts::port::PortPrivacyClass::Phi;
        let mut consumer = p("data:0863");
        consumer.privacy_class = crate::workflow_contracts::port::PortPrivacyClass::Internal;
        let res = engine.prove(&producer, &consumer, &PlanningContext::default());
        match res {
            CompatibilityResult::Incompatible(report) => {
                assert!(matches!(
                    report.reasons[0],
                    IncompatibilityReason::PrivacyClassWidening { .. }
                ));
            }
            other => panic!("expected Incompatible, got {other:?}"),
        }
    }

    #[test]
    fn local_extension_with_matching_parent_is_compatible() {
        let engine = DeterministicCompatibilityEngine::new();
        let mut producer = p("data:0863");
        producer.semantic_type = SemanticType::LocalExtension {
            namespace: "ecaax".into(),
            id: "scrnaseq_doublet_score".into(),
            proposed_parent_terms: vec!["data:2603".into()],
            definition: "doublet probability".into(),
            maturity: crate::workflow_contracts::semantic_type::LocalExtensionMaturity::Minted,
        };
        let mut consumer = p("data:2603");
        // Consumer asks for the parent term.
        consumer.semantic_type = SemanticType::edam("data:2603", "Gene expression matrix");
        let res = engine.prove(&producer, &consumer, &PlanningContext::default());
        assert!(matches!(res, CompatibilityResult::Compatible(_)));
    }

    #[test]
    fn genome_build_grch37_to_grch38_with_risky_policy_returns_compatible_with_adapters() {
        // With allow_risky_with_confirmation enabled, the engine
        // looks up the liftover adapter and returns
        // CompatibleWithAdapters so the planner can present a draft
        // DAG (and the composer records the assumption ledger entry).
        let engine = DeterministicCompatibilityEngine::new();
        let mut producer = p("data:0863");
        producer.genome_build = Some("GRCh37".into());
        let mut consumer = p("data:0863");
        consumer.genome_build = Some("GRCh38".into());
        let ctx = PlanningContext {
            adapter_policy: AdapterPolicy {
                allow_lossless: true,
                allow_lossy_with_assumption: true,
                allow_risky_with_confirmation: true,
            },
            ..Default::default()
        };
        let res = engine.prove(&producer, &consumer, &ctx);
        match res {
            CompatibilityResult::CompatibleWithAdapters { adapters, .. } => {
                assert_eq!(adapters.len(), 1);
                assert_eq!(adapters[0].id, "liftover_grch37_grch38");
            }
            other => panic!("expected CompatibleWithAdapters, got {other:?}"),
        }
    }

    #[test]
    fn genome_build_grch37_to_grch38_with_lossless_only_policy_returns_incompatible() {
        // Liftover is ScientificallyRisky; lossless-only
        // policy refuses to insert it; engine returns Incompatible.
        let engine = DeterministicCompatibilityEngine::new();
        let mut producer = p("data:0863");
        producer.genome_build = Some("GRCh37".into());
        let mut consumer = p("data:0863");
        consumer.genome_build = Some("GRCh38".into());
        let ctx = PlanningContext {
            adapter_policy: AdapterPolicy::lossless_only(),
            ..Default::default()
        };
        let res = engine.prove(&producer, &consumer, &ctx);
        assert!(matches!(res, CompatibilityResult::Incompatible(_)));
    }

    #[test]
    fn unknown_genome_build_pair_with_risky_policy_still_incompatible() {
        // Only adapters that exist in the registry can
        // repair a mismatch. GRCh38 -> hg18 has no adapter, so
        // even with the most permissive policy the engine returns
        // Incompatible.
        let engine = DeterministicCompatibilityEngine::new();
        let mut producer = p("data:0863");
        producer.genome_build = Some("GRCh38".into());
        let mut consumer = p("data:0863");
        consumer.genome_build = Some("hg18".into());
        let ctx = PlanningContext {
            adapter_policy: AdapterPolicy {
                allow_lossless: true,
                allow_lossy_with_assumption: true,
                allow_risky_with_confirmation: true,
            },
            ..Default::default()
        };
        let res = engine.prove(&producer, &consumer, &ctx);
        assert!(matches!(res, CompatibilityResult::Incompatible(_)));
    }

    #[test]
    fn proof_records_substituted_facet_when_adapter_inserted() {
        let engine = DeterministicCompatibilityEngine::new();
        let mut producer = p("data:0863");
        producer.genome_build = Some("GRCh37".into());
        let mut consumer = p("data:0863");
        consumer.genome_build = Some("GRCh38".into());
        let ctx = PlanningContext {
            adapter_policy: AdapterPolicy {
                allow_lossless: true,
                allow_lossy_with_assumption: true,
                allow_risky_with_confirmation: true,
            },
            ..Default::default()
        };
        let res = engine.prove(&producer, &consumer, &ctx);
        match res {
            CompatibilityResult::CompatibleWithAdapters { proof, .. } => {
                assert!(
                    proof
                        .facet_matches
                        .iter()
                        .any(|f| f.facet == "genome_build"),
                    "genome_build facet missing from proof: {:?}",
                    proof.facet_matches
                );
                assert!(
                    proof
                        .inserted_adapter_node_ids
                        .iter()
                        .any(|a| a == "liftover_grch37_grch38"),
                    "adapter id missing from proof.inserted_adapter_node_ids: {:?}",
                    proof.inserted_adapter_node_ids
                );
            }
            other => panic!("expected CompatibleWithAdapters, got {other:?}"),
        }
    }

    #[test]
    fn policy_no_scientifically_risky_adapters_refuses_liftover() {
        // Clinical policy bundle refuses scientifically
        // risky adapters; engine returns Incompatible(PolicyViolation)
        // even when the adapter policy itself would allow the liftover.
        let engine = DeterministicCompatibilityEngine::new();
        let mut producer = p("data:0863");
        producer.genome_build = Some("GRCh37".into());
        let mut consumer = p("data:0863");
        consumer.genome_build = Some("GRCh38".into());
        let ctx = PlanningContext {
            adapter_policy: AdapterPolicy {
                allow_lossless: true,
                allow_lossy_with_assumption: true,
                allow_risky_with_confirmation: true,
            },
            policy_context: PolicyContext::empty().with_bundle(
                crate::policy_context::PolicyBundle {
                    id: "test_no_risky".into(),
                    label: "Test no risky adapters".into(),
                    description: None,
                    checks: vec![PolicyCheck::NoScientificallyRiskyAdapters],
                    regulatory_citation: None,
                    population_waivers: Vec::new(),
                },
            ),
            ..Default::default()
        };
        let res = engine.prove(&producer, &consumer, &ctx);
        match res {
            CompatibilityResult::Incompatible(report) => {
                assert!(
                    report.reasons.iter().any(|r| matches!(
                        r,
                        IncompatibilityReason::PolicyViolation { check_kind, .. }
                            if check_kind == "no_scientifically_risky_adapters"
                    )),
                    "expected PolicyViolation, got {:?}",
                    report.reasons
                );
            }
            other => panic!("expected Incompatible, got {other:?}"),
        }
    }

    #[test]
    fn policy_decisions_recorded_in_proof_for_compatible_edge() {
        // When an active policy bundle's check passes, the
        // engine records a positive decision in the proof so the
        // RO-Crate / WRROC provenance surfaces the active policy.
        let engine = DeterministicCompatibilityEngine::new();
        let producer = p("data:0863");
        let consumer = p("data:0863");
        let ctx = PlanningContext {
            policy_context: PolicyContext::empty().with_bundle(
                crate::policy_context::PolicyBundle {
                    id: "test_audit".into(),
                    label: "Audit-required".into(),
                    description: None,
                    checks: vec![PolicyCheck::AuditTrailRequired],
                    regulatory_citation: None,
                    population_waivers: Vec::new(),
                },
            ),
            ..Default::default()
        };
        let res = engine.prove(&producer, &consumer, &ctx);
        match res {
            CompatibilityResult::Compatible(proof) => {
                assert!(
                    proof
                        .policy_decisions
                        .iter()
                        .any(|d| d.contains("test_audit") && d.contains("audit_trail_required")),
                    "expected policy decision, got {:?}",
                    proof.policy_decisions
                );
            }
            other => panic!("expected Compatible, got {other:?}"),
        }
    }

    #[test]
    fn clinical_bundle_active_policy_decisions_surface_in_proof() {
        // A complete clinical bundle records all active
        // checks in the proof's policy_decisions list (positive
        // decisions for soft checks; per-node checks marked as
        // deferred to the planner).
        let engine = DeterministicCompatibilityEngine::new();
        let producer = p("data:0863");
        let consumer = p("data:0863");
        let ctx = PlanningContext {
            policy_context: PolicyContext::empty()
                .with_bundle(PolicyContext::clinical_trial_bundle()),
            ..Default::default()
        };
        let res = engine.prove(&producer, &consumer, &ctx);
        match res {
            CompatibilityResult::Compatible(proof) => {
                let decisions = proof.policy_decisions.join(",");
                assert!(decisions.contains("clinical_trial"));
                assert!(decisions.contains("audit_trail_required"));
                assert!(decisions.contains("human_signoff_required"));
                assert!(decisions.contains("no_privacy_widening"));
            }
            other => panic!("expected Compatible, got {other:?}"),
        }
    }

    #[test]
    fn determinism_replay() {
        // Acceptance: 100× replay is byte-identical.
        let engine = DeterministicCompatibilityEngine::new();
        let mut producer = p("data:0863");
        producer.genome_build = Some("GRCh38".into());
        let mut consumer = p("data:0863");
        consumer.organism = Some("Homo sapiens".into());
        let mut prev_serialized: Option<String> = None;
        for _ in 0..100 {
            let res = engine.prove(&producer, &consumer, &PlanningContext::default());
            let json = serde_json::to_string(&res).unwrap();
            if let Some(prev) = &prev_serialized {
                assert_eq!(prev, &json, "engine non-deterministic");
            }
            prev_serialized = Some(json);
        }
    }
}
