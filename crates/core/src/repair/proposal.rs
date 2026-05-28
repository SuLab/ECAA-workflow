//! `RepairProposal` + `DagModification` + ancillary types.
//!
//! A [`RepairProposal`] is the deterministic
//! recommendation produced by a [`super::strategy::RepairStrategy`]
//! for a single unsatisfied [`RepairGap`]. The planner never mutates
//! the DAG from a proposal unless the proposal's `risk_class` is at
//! or below `PlanningContext::auto_attempt_risk_threshold` — that
//! invariant is the F20 contract.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::workflow_contracts::evidence::Assumption;
use crate::workflow_contracts::port::PortContract;
use crate::workflow_contracts::task_node::TaskNode;

use super::strategy::GapKind;

/// Structured gap detail consumed by the repair registry. Mirrors the
/// stringly-keyed `MeetResult::Disconnected { gaps }` entries but adds
/// the producer/consumer port references + facet-mismatch breakdown
/// that strategies need to decide whether they can repair the gap.
///
/// Built by `MeetResult` (when the meet records that producer P didn't
/// unify with consumer C on a specific input port) and by the planner
/// for "no producer at all" gaps where `producer_node` is `None`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, TS, schemars::JsonSchema)]
#[ts(export)]
pub struct RepairGap {
    /// Stable id (deterministic: derived from the
    /// `(consumer_node, consumer_port, kind)` tuple).
    pub id: String,
    /// Free-text statement (matches `GapReport::statement`).
    pub statement: String,
    /// Coarse-grained kind discriminator. Strategies filter by this
    /// before inspecting facet detail.
    pub kind: GapKind,
    /// Consumer (downstream) node id whose input port is unsatisfied.
    pub consumer_node: String,
    /// Consumer port name on `consumer_node`.
    pub consumer_port: String,
    /// Producer (upstream) node id when a candidate producer was
    /// considered but ruled out. `None` when no producer was even
    /// reachable (`MissingProducer` kind).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub producer_node: Option<String>,
    /// Producer port name when `producer_node` is set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub producer_port: Option<String>,
    /// Facet-by-facet mismatch breakdown. Empty when the gap is purely
    /// structural (no producer reachable).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub facet_mismatches: Vec<FacetMismatch>,
}

/// One facet that diverged between producer + consumer ports.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, TS, schemars::JsonSchema)]
#[ts(export)]
pub struct FacetMismatch {
    /// Facet name (`genome_build`, `annotation_version`, `strandedness`,
    /// `units`, `normalization_state`, `coordinate_system`, …).
    pub facet: String,
    /// Producer's value (may be empty string when unset).
    pub producer_value: String,
    /// Consumer's required value (may be empty string when unset).
    pub consumer_value: String,
}

// `Hash` over `RepairGap` and `GapKind` is needed when callers tunnel
// gaps through `BTreeSet` / `HashMap` — not required by the registry
// itself but routinely useful in tests + adapter glue. Derived via
// the `ts-rs` round-trip output rather than blanket-derived above so
// optional fields produce the same `Hash` regardless of presence.

/// Final deliverable a strategy produces. Wrapped by the planner with
/// substrate emission (`VerifierDecision::RepairProposed`) and routed
/// to the UI's `RepairProposalCard` for SME accept/reject/defer.
///
/// Note: derives `PartialEq` but not `Eq` because `modification:
/// DagModification` references `TaskNode`/`PortContract` which no
/// longer implement `Eq` (v4 P6 maturity field). Proposal-level dedup
/// keys on `id` instead.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS, schemars::JsonSchema)]
#[ts(export)]
pub struct RepairProposal {
    /// Stable proposal id (deterministic hash of strategy + gap +
    /// ctx_snapshot_hash). Same input + same context produces the
    /// same id across runs — required by F20's determinism contract.
    pub id: String,
    /// Strategy that produced this proposal
    /// (`RepairStrategy::id()`).
    pub strategy_id: String,
    /// Originating gap id.
    pub gap_id: String,
    /// Concrete DAG mutation the strategy recommends.
    pub modification: DagModification,
    /// Risk class — drives the planner's auto-attempt gate. F20:
    /// `MediumUserGated` + `HighCredentialedReview` MUST emit but
    /// MUST NOT mutate the DAG.
    pub risk_class: RepairRiskClass,
    /// Assumptions the repair would introduce (e.g.
    /// `"liftover_introduces_coordinate_uncertainty"`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub generated_assumptions: Vec<Assumption>,
    /// Credential classes the SME must present at accept time
    /// (`"bioinformatics_lead"`, `"clinical_lead"`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub required_credentials: Vec<String>,
    /// Human-readable rationale shown alongside Accept / Reject /
    /// Defer in the UI.
    pub rationale: String,
    /// Stable hash of the planning context at proposal time. The
    /// accept endpoint refuses proposals whose `ctx_snapshot_hash`
    /// no longer matches the current planning context — guards
    /// against accepting a proposal computed against stale intake.
    pub ctx_snapshot_hash: String,
}

/// What kind of mutation the proposal would apply. The variants are
/// intentionally narrow — strategies pick the closest shape rather
/// than smuggling arbitrary edits through a free-form blob.
///
/// Note: `PartialEq` but not `Eq` because `TaskNode` and `PortContract`
/// can carry a `SemanticType::LocalExtension` whose `maturity` field
/// can be `LocalExtensionMaturity::GraduationCandidate { success_rate:
/// F32,.. }`. Callers that need set-style dedup compare on node id.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS, schemars::JsonSchema)]
#[ts(export)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DagModification {
    /// Splice a converter node onto the edge from `source_port` to
    /// `sink_port`. Used by `insert_gzip_decompression` and
    /// `insert_sort_index`.
    InsertConverter {
        /// Converter node.
        converter_node: TaskNode,
        /// Source port.
        source_port: PortRef,
        /// Sink port.
        sink_port: PortRef,
    },
    /// Splice a liftover adapter onto `target_edge` translating from
    /// `from_build` → `to_build`. Used by `insert_liftover`.
    InsertLiftover {
        /// From build.
        from_build: String,
        /// To build.
        to_build: String,
        /// Target edge.
        target_edge: EdgeRef,
    },
    /// Surface a `BlockerKind::UnsetMethod` (or analogue) requesting
    /// the SME supply the named field. No DAG mutation today — the
    /// SME's reply re-enters the planner with the field populated.
    RequestMissingMetadata {
        /// Field.
        field: String,
        /// Applies to node.
        applies_to_node: String,
    },
    /// Swap one producer atom for another whose output unifies with
    /// the consumer's input. Used by `substitute_compatible_producer`.
    SubstituteProducer { remove: String, add: TaskNode },
    /// Issue a registry query for a producer matching `criteria`.
    /// Used by `query_registry_for_match`. The accept endpoint
    /// dispatches the query; the result re-enters the planner.
    QueryRegistry {
        /// Criteria.
        criteria: RegistryQuery,
        /// Target gap.
        target_gap: String,
    },
    /// Rewrite an upstream node's contract — narrows + tightens the
    /// declared port shape rather than inserting an adapter.
    /// Used by `rewrite_upstream_contract`; high-risk because it
    /// changes the producer's promise rather than papering over a
    /// mismatch.
    RewriteContract {
        /// Node.
        node: String,
        /// New contract.
        new_contract: PortContract,
        /// Applies to.
        applies_to: PortRole,
    },
}

/// One endpoint of an edge — `(node_id, port_name)` pair. Used in
/// `DagModification::InsertConverter` / `InsertLiftover`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, TS, schemars::JsonSchema)]
#[ts(export)]
pub struct PortRef {
    /// Node id.
    pub node_id: String,
    /// Port name.
    pub port_name: String,
}

/// Producer → consumer edge reference.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, TS, schemars::JsonSchema)]
#[ts(export)]
pub struct EdgeRef {
    /// From.
    pub from: PortRef,
    /// To.
    pub to: PortRef,
}

/// Lookup criteria for `DagModification::QueryRegistry`. Used by the
/// repair-strategy `query_registry_for_match` when an existing atom
/// is suspected to exist but wasn't reachable from the search.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, TS, schemars::JsonSchema)]
#[ts(export)]
pub struct RegistryQuery {
    /// Semantic-type IRI to match against `atom.outputs[*].semantic_type`.
    pub semantic_type: String,
    /// Optional modality bucket to filter on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub modality: Option<String>,
}

/// Which side of an edge a `RewriteContract` targets. Inputs are the
/// consumer's required shape; outputs are the producer's promised
/// shape.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, TS, schemars::JsonSchema)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum PortRole {
    /// Input variant.
    Input,
    /// Output variant.
    Output,
}

/// Risk classification. Drives the planner's auto-attempt gate
/// (F20 invariant). Ordered LOW < MEDIUM < HIGH so `proposal.risk_class
/// <= ctx.auto_attempt_risk_threshold` is the canonical
/// "may auto-apply" check.
///
/// `Default` is `LowAutoAttempt` so the planner's
/// `auto_attempt_risk_threshold` defaults to "only auto-apply lossless
/// fixes" without forbidding the whole repair path (the most
/// conservative SAFE default — set higher only when the operator
/// explicitly opts in).
#[derive(
    Debug,
    Clone,
    Copy,
    Default,
    Serialize,
    Deserialize,
    PartialEq,
    Eq,
    TS,
    PartialOrd,
    Ord,
    Hash,
    schemars::JsonSchema,
)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum RepairRiskClass {
    /// Lossless / well-known mechanical fix (e.g. gzip decompression
    /// converter, BAM index regeneration). Safe to auto-apply.
    #[default]
    LowAutoAttempt,
    /// User-gated fix — the SME sees the proposal but must accept it
    /// before the DAG mutates. Default for liftover, batch correction.
    MediumUserGated,
    /// Credentialed-reviewer-gated fix. Requires explicit credentials
    /// (clinical_lead, regulatory_officer). The planner emits but
    /// never auto-applies.
    HighCredentialedReview,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflow_contracts::evidence::AssumptionLedger;
    use crate::workflow_contracts::task_node::TaskNode;

    #[test]
    fn risk_class_ordering_low_lt_medium_lt_high() {
        assert!(RepairRiskClass::LowAutoAttempt < RepairRiskClass::MediumUserGated);
        assert!(RepairRiskClass::MediumUserGated < RepairRiskClass::HighCredentialedReview);
    }

    #[test]
    fn proposal_round_trips_via_serde() {
        let p = RepairProposal {
            id: "repair:test:1".into(),
            strategy_id: "insert_liftover".into(),
            gap_id: "g1".into(),
            modification: DagModification::InsertLiftover {
                from_build: "GRCh37".into(),
                to_build: "GRCh38".into(),
                target_edge: EdgeRef {
                    from: PortRef {
                        node_id: "n1".into(),
                        port_name: "p1".into(),
                    },
                    to: PortRef {
                        node_id: "n2".into(),
                        port_name: "p2".into(),
                    },
                },
            },
            risk_class: RepairRiskClass::HighCredentialedReview,
            generated_assumptions: Vec::new(),
            required_credentials: vec!["bioinformatics_lead".into()],
            rationale: "lift across builds".into(),
            ctx_snapshot_hash: "h".into(),
        };
        let json = serde_json::to_string(&p).unwrap();
        let back: RepairProposal = serde_json::from_str(&json).unwrap();
        assert_eq!(p, back);
    }

    #[test]
    fn substitute_modification_round_trips() {
        let m = DagModification::SubstituteProducer {
            remove: "old".into(),
            add: TaskNode::skeleton("new", "x"),
        };
        let json = serde_json::to_string(&m).unwrap();
        let back: DagModification = serde_json::from_str(&json).unwrap();
        assert_eq!(m, back);
        // Use AssumptionLedger to make the import non-dead (mirrors
        // strategy modules that ledger assumptions onto a repaired DAG).
        let _ = AssumptionLedger::default();
    }
}
