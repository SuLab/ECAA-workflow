//! Edge contracts and compatibility proofs per design §1 + §4.
//!
//! The composer wires the compatibility engine to validate
//! producer→consumer reachability. Each `EdgeContract` carries an
//! inline `CompatibilityProof` so consumers don't need a side lookup
//! to understand why an edge exists.
//!
//! At emit time `CompatibilityProof` entries are lowered into
//! `runtime/proofs.jsonl`, registered as an RO-Crate `CreativeWork`
//! entity. The harness ignores the sidecar at execution time.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use super::chain_of_custody::ChainOfCustody;
use super::evidence::{AssumptionRef, ValidatorRef};

/// Typed discriminant for a composed-DAG edge. Replaces the prior
/// substring-keyed reject (`warnings.contains("incompatible")`) with a
/// pass/fail axis the scorer can branch on directly.
///
/// `Unproven` is the default so any legacy `runtime/proofs.jsonl` row
/// or hand-written edge that omits `kind` is treated as the *strictest
/// failing* case — it is never silently waved through the gate.
/// Defaulting to `TypedDataFlow` would re-open the loophole.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, TS, schemars::JsonSchema)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum EdgeKind {
    /// Producer output and consumer input unified losslessly
    /// (`CompatibilityResult::Compatible`).
    TypedDataFlow,
    /// Unification required one or more adapters
    /// (`CompatibilityResult::CompatibleWithAdapters`).
    AdapterMediated,
    /// No port pair unified, but the edge is a workflow-ordering
    /// relation — either an archetype author declared it via
    /// `ordering_only_edges`, or it is a synthesis-site structural
    /// edge (discover/survey/multi-branch/validator/report wiring).
    /// Non-blocking in `Draft`; rejected in `Production` (WG3).
    OrderingOnly,
    /// No port pair unified and the edge is NOT a declared ordering
    /// exemption (or the producer's stable type is empty). The gate
    /// rejects any DAG carrying an `Unproven` edge.
    Unproven,
}

impl Default for EdgeKind {
    fn default() -> Self {
        EdgeKind::Unproven
    }
}

impl EdgeKind {
    /// Strength rank for merging multiple port-level edges that share the
    /// same `(from_node, to_node)` pair into the single node-pair edge the
    /// core proofs.jsonl emits. A higher rank is a stronger compatibility
    /// claim: if ANY port between two nodes is a typed data flow, the
    /// node-level dependency is at least that well-typed, so the strongest
    /// port-kind wins. Total order: `TypedDataFlow > AdapterMediated >
    /// OrderingOnly > Unproven`.
    pub fn strength_rank(self) -> u8 {
        match self {
            EdgeKind::TypedDataFlow => 3,
            EdgeKind::AdapterMediated => 2,
            EdgeKind::OrderingOnly => 1,
            EdgeKind::Unproven => 0,
        }
    }

    /// Fold another kind in, keeping the stronger of the two.
    pub fn merge_strongest(self, other: EdgeKind) -> EdgeKind {
        if other.strength_rank() > self.strength_rank() {
            other
        } else {
            self
        }
    }
}

/// Project a `WorkflowDag`'s port-level edge kinds onto a node-pair map
/// (`(from_node, to_node) -> strongest EdgeKind`). The core emit path holds
/// only a `DAG` (no typed edges), so callers that DO have a `WorkflowDag`
/// (CLI build/intake, conversation) pass this map through `EmitConfig` so
/// `runtime/proofs.jsonl`'s synthesized dependency edges carry the real
/// lifted kind instead of the strict `Unproven` placeholder (WG4b).
pub fn edge_kind_map_from_edges(
    edges: &[EdgeContract],
) -> std::collections::BTreeMap<(String, String), EdgeKind> {
    let mut map: std::collections::BTreeMap<(String, String), EdgeKind> =
        std::collections::BTreeMap::new();
    for e in edges {
        let key = (e.from_node.clone(), e.to_node.clone());
        let entry = map.entry(key).or_insert(EdgeKind::Unproven);
        *entry = entry.merge_strongest(e.kind);
    }
    map
}

/// Serde default for [`EdgeContract::kind`]. A named fn (not the derive)
/// so the strictest case applies to legacy rows missing the field.
fn default_edge_kind() -> EdgeKind {
    EdgeKind::Unproven
}

/// One edge in a `WorkflowDag`. Connects a producer port to a
/// consumer port and carries the compatibility proof or report
/// explaining why the edge exists.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq, TS, schemars::JsonSchema)]
#[ts(export)]
pub struct EdgeContract {
    /// Producer task node id.
    pub from_node: String,
    /// Producer port name (must be a member of the producer's
    /// `outputs`).
    pub from_port: String,
    /// Consumer task node id.
    pub to_node: String,
    /// Consumer port name (must be a member of the consumer's
    /// `inputs`).
    pub to_port: String,
    /// Compatibility proof. Carried inline so consumers don't need
    /// a side lookup to render "why this edge exists."
    pub proof: CompatibilityProof,
    /// Typed edge classification. Defaults to [`EdgeKind::Unproven`] so
    /// legacy proofs.jsonl rows (and any edge constructed before this
    /// field existed) deserialize to the strictest failing case.
    #[serde(default = "default_edge_kind")]
    pub kind: EdgeKind,
    /// Chain-of-custody for the edge when one or both
    /// endpoints carry suppressed content. `None` for ordinary
    /// non-suppressed edges. Older records without the field
    /// deserialize cleanly.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub chain_of_custody: Option<ChainOfCustody>,
    /// When set, this edge is one member of a mutually-exclusive one-of
    /// input group named here; sibling members are alternatives resolved
    /// to a single authoritative edge by observed-provenance (Phase 2).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub mutually_exclusive_group: Option<String>,
}

impl EdgeContract {
    /// Build a structural splice edge bridging two nodes whose
    /// intermediate atoms were removed by an intake-fact gate. The
    /// proof is a sentinel — `intake_fact_splice` rationale, empty
    /// adapter list — because the surviving endpoints' typed
    /// inputs/outputs may not directly match. Runtime reconciliation
    /// of the typed boundary happens via `runtime/inputs/`.
    pub fn synthetic_splice(from_node: String, to_node: String) -> Self {
        Self {
            from_node,
            from_port: "splice".to_string(),
            to_node,
            to_port: "splice".to_string(),
            proof: CompatibilityProof {
                producer_type: "splice".to_string(),
                consumer_type: "splice".to_string(),
                rationale: Some(
                    "Splice edge inserted to preserve reachability after an \
                     intake-fact post-filter removed the original chain segment. \
                     Typed I/O boundary is reconciled by the agent at runtime \
                     against runtime/inputs/."
                        .to_string(),
                ),
                ..CompatibilityProof::default()
            },
            // A splice is a structural reachability bridge, not a typed
            // data flow, so it is an ordering-only edge.
            kind: EdgeKind::OrderingOnly,
            chain_of_custody: None,
            mutually_exclusive_group: None,
        }
    }
}

/// Proof that a producer port satisfies a consumer port. The
/// compatibility engine produces these; they carry adapter steps,
/// assumption references, and policy decisions inline.
#[derive(
    Debug, Clone, Serialize, Deserialize, PartialEq, Eq, TS, Default, schemars::JsonSchema,
)]
#[ts(export)]
pub struct CompatibilityProof {
    /// Producer's output type stable id (mirrors
    /// `SemanticType::stable_id`).
    pub producer_type: String,
    /// Consumer's input type stable id.
    pub consumer_type: String,
    /// Ontology subsumption path the engine traversed (parent IRIs
    /// from producer to consumer). Empty when types are identical.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ontology_subsumption_path: Vec<String>,
    /// Per-facet match decisions (modality, organism, genome build,
    /// etc.). Empty for ports with no rich facets.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub facet_matches: Vec<FacetMatch>,
    /// Format conversions performed (e.g. `gzip` → ungzipped). Non-empty
    /// when adapters are inserted between producer and consumer.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub format_conversions: Vec<String>,
    /// Adapter task node ids inserted between producer and consumer.
    /// One id per adapter step in chain order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inserted_adapter_node_ids: Vec<String>,
    /// Validators that must pass before execution. Resolved by the
    /// claim_verifier at runtime.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub required_validators: Vec<ValidatorRef>,
    /// Warnings worth surfacing in the UI but not blocking.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
    /// Assumption ledger entries this edge depends on. Populated
    /// by the planner when type or facet compatibility is uncertain.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub assumptions: Vec<AssumptionRef>,
    /// Policy decisions recorded by the compatibility engine. E.g.
    /// `phi_propagation_allowed` or `clinical_gate_passed`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub policy_decisions: Vec<String>,
    /// Free-text rationale for human reading.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub rationale: Option<String>,
    /// Supporting evidence (registry snapshot ids, validator
    /// outputs, etc.). Used for replay determinism tests.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<ProofEvidence>,
}

/// Per-facet match decision in a `CompatibilityProof`. Stable shape
/// so UI rendering is consistent across facets.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, TS, schemars::JsonSchema)]
#[ts(export)]
pub struct FacetMatch {
    /// Facet name (`genome_build`, `organism`, `modality`, etc.).
    pub facet: String,
    /// Producer's value (free-form because facet types are
    /// heterogeneous).
    pub producer: String,
    /// Consumer's value.
    pub consumer: String,
    /// `Exact` / `Subtype` / `Substituted` / `Unknown`.
    pub kind: FacetMatchKind,
    /// Optional rationale (e.g. "GRCh37 → GRCh38 via UCSC
    /// liftover").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub rationale: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, TS, schemars::JsonSchema)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
/// FacetMatchKind discriminant.
pub enum FacetMatchKind {
    /// Producer and consumer values are identical.
    Exact,
    /// Producer is a subtype of consumer (e.g. "Homo sapiens" of
    /// "mammal").
    Subtype,
    /// Producer was substituted to match consumer (e.g. liftover).
    /// Implies an adapter was inserted.
    Substituted,
    /// Engine could not decide; assumption ledger entry created.
    Unknown,
}

/// Supporting evidence for a `CompatibilityProof`. Stable enough to
/// replay in determinism tests.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, TS, schemars::JsonSchema)]
#[ts(export)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ProofEvidence {
    /// Registry snapshot id (atom registry, ontology version).
    RegistrySnapshot {
        /// Registry.
        registry: String,
        /// Snapshot id.
        snapshot_id: String,
    },
    /// Validator output reference. Resolved at provenance-emit time.
    ValidatorRun {
        /// Validator id.
        validator_id: String,
        /// Result ref.
        result_ref: String,
    },
    /// Free-form citation (e.g. EDAM term URI + version).
    Citation { uri: String, label: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_default_proof() {
        let p = CompatibilityProof::default();
        let json = serde_json::to_string(&p).unwrap();
        let back: CompatibilityProof = serde_json::from_str(&json).unwrap();
        assert_eq!(p, back);
    }

    #[test]
    fn round_trip_full_proof() {
        let p = CompatibilityProof {
            producer_type: "data:0863".into(),
            consumer_type: "data:0863".into(),
            ontology_subsumption_path: vec!["data:0006".into()],
            facet_matches: vec![FacetMatch {
                facet: "genome_build".into(),
                producer: "GRCh38".into(),
                consumer: "GRCh38".into(),
                kind: FacetMatchKind::Exact,
                rationale: None,
            }],
            format_conversions: vec![],
            inserted_adapter_node_ids: vec![],
            required_validators: vec![],
            warnings: vec![],
            assumptions: vec![],
            policy_decisions: vec!["phi_propagation_allowed".into()],
            rationale: Some("Both ports are GRCh38 BAM with identical coordinate system".into()),
            evidence: vec![ProofEvidence::RegistrySnapshot {
                registry: "edam".into(),
                snapshot_id: "1.25-20251112T1620Z".into(),
            }],
        };
        let yaml = serde_yaml_ng::to_string(&p).unwrap();
        let back: CompatibilityProof = serde_yaml_ng::from_str(&yaml).unwrap();
        assert_eq!(p, back);
    }

    #[test]
    fn edge_round_trip() {
        let e = EdgeContract {
            from_node: "align_reads".into(),
            from_port: "bam".into(),
            to_node: "call_variants".into(),
            to_port: "bam".into(),
            proof: CompatibilityProof::default(),
            kind: EdgeKind::TypedDataFlow,
            chain_of_custody: None,
            mutually_exclusive_group: None,
        };
        let json = serde_json::to_string(&e).unwrap();
        let back: EdgeContract = serde_json::from_str(&json).unwrap();
        assert_eq!(e, back);
    }

    /// v3 P5 — `chain_of_custody` is omitted at serialize time when
    /// `None` so legacy records remain byte-identical to the baseline.
    #[test]
    fn edge_round_trip_omits_custody_when_none() {
        let e = EdgeContract {
            from_node: "align_reads".into(),
            from_port: "bam".into(),
            to_node: "call_variants".into(),
            to_port: "bam".into(),
            proof: CompatibilityProof::default(),
            kind: EdgeKind::TypedDataFlow,
            chain_of_custody: None,
            mutually_exclusive_group: None,
        };
        let json = serde_json::to_string(&e).unwrap();
        assert!(!json.contains("chain_of_custody"), "got: {json}");
    }

    #[test]
    fn edge_kind_round_trips_each_variant() {
        for k in [
            EdgeKind::TypedDataFlow,
            EdgeKind::AdapterMediated,
            EdgeKind::OrderingOnly,
            EdgeKind::Unproven,
        ] {
            let json = serde_json::to_string(&k).unwrap();
            let back: EdgeKind = serde_json::from_str(&json).unwrap();
            assert_eq!(k, back);
        }
    }

    #[test]
    fn edge_kind_serializes_snake_case() {
        assert_eq!(
            serde_json::to_string(&EdgeKind::TypedDataFlow).unwrap(),
            "\"typed_data_flow\""
        );
        assert_eq!(
            serde_json::to_string(&EdgeKind::OrderingOnly).unwrap(),
            "\"ordering_only\""
        );
    }

    #[test]
    fn edge_kind_default_is_unproven() {
        assert_eq!(EdgeKind::default(), EdgeKind::Unproven);
    }

    /// WG4b — `merge_strongest` keeps the stronger compatibility claim so a
    /// node pair with any typed-data-flow port lifts to TypedDataFlow.
    #[test]
    fn edge_kind_merge_strongest_keeps_stronger() {
        assert_eq!(
            EdgeKind::Unproven.merge_strongest(EdgeKind::TypedDataFlow),
            EdgeKind::TypedDataFlow
        );
        assert_eq!(
            EdgeKind::TypedDataFlow.merge_strongest(EdgeKind::OrderingOnly),
            EdgeKind::TypedDataFlow
        );
        assert_eq!(
            EdgeKind::OrderingOnly.merge_strongest(EdgeKind::AdapterMediated),
            EdgeKind::AdapterMediated
        );
        assert_eq!(
            EdgeKind::OrderingOnly.merge_strongest(EdgeKind::Unproven),
            EdgeKind::OrderingOnly
        );
    }

    /// WG4b — projecting port-level edges onto a node-pair map collapses
    /// multiple ports between the same pair to the strongest kind.
    #[test]
    fn edge_kind_map_collapses_to_strongest_per_pair() {
        let mk = |fp: &str, kind: EdgeKind| EdgeContract {
            from_node: "a".into(),
            from_port: fp.into(),
            to_node: "b".into(),
            to_port: "in".into(),
            proof: CompatibilityProof::default(),
            kind,
            chain_of_custody: None,
            mutually_exclusive_group: None,
        };
        let edges = vec![
            mk("ordering", EdgeKind::OrderingOnly),
            mk("bam", EdgeKind::TypedDataFlow),
        ];
        let map = edge_kind_map_from_edges(&edges);
        assert_eq!(
            map.get(&("a".to_string(), "b".to_string())).copied(),
            Some(EdgeKind::TypedDataFlow),
            "the strongest port kind must win for the node pair"
        );
    }

    /// A legacy proofs.jsonl row without a `kind` field deserializes to
    /// the strictest failing case (`Unproven`), never silently passing.
    #[test]
    fn edge_contract_legacy_row_defaults_kind_to_unproven() {
        let legacy = r#"{
            "from_node": "a", "from_port": "out",
            "to_node": "b", "to_port": "in",
            "proof": {"producer_type": "data:0863", "consumer_type": "data:0863"}
        }"#;
        let edge: EdgeContract = serde_json::from_str(legacy).unwrap();
        assert_eq!(edge.kind, EdgeKind::Unproven);
    }

    /// v3 P5 — `chain_of_custody` round-trips when populated.
    #[test]
    fn edge_round_trip_carries_custody_when_some() {
        use super::super::chain_of_custody::{AuditorProcedure, ChainOfCustody, SuppressionClass};
        let e = EdgeContract {
            from_node: "ingest_user_prose".into(),
            from_port: "prose".into(),
            to_node: "classify_intake".into(),
            to_port: "prose".into(),
            proof: CompatibilityProof::default(),
            kind: EdgeKind::OrderingOnly,
            chain_of_custody: Some(ChainOfCustody::new(
                SuppressionClass::UserSuppliedFreeText,
                "ingestion_safety::quarantine",
                "prompt_injection:v1".to_string(),
                AuditorProcedure::PermanentlyDeleted {
                    deletion_authority: "operator".into(),
                    deletion_id: "del-1".into(),
                },
                &crate::clock::WallClock,
            )),
            mutually_exclusive_group: None,
        };
        let json = serde_json::to_string(&e).unwrap();
        let back: EdgeContract = serde_json::from_str(&json).unwrap();
        assert_eq!(e, back);
    }

    #[test]
    fn edge_mutually_exclusive_group_defaults_none_and_serializes_skipped() {
        let e = EdgeContract { mutually_exclusive_group: None, ..EdgeContract::default() };
        let json = serde_json::to_string(&e).unwrap();
        assert!(!json.contains("mutually_exclusive_group"), "absent tag must not serialize");
        let tagged = EdgeContract { mutually_exclusive_group: Some("counts".into()), ..EdgeContract::default() };
        let back: EdgeContract = serde_json::from_str(&serde_json::to_string(&tagged).unwrap()).unwrap();
        assert_eq!(back.mutually_exclusive_group.as_deref(), Some("counts"));
    }
}
