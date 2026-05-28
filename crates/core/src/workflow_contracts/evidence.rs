//! Evidence, validators, assumption ledger, and risk class.
//!
//! Assumption ledger entries are persisted via
//! `decision_log::DecisionRecord` (`DecisionType::AssumptionRecorded`
//! / `AssumptionResolved`). Validator references are resolved by the
//! harness verify endpoint through the `claim_extractor` /
//! `claim_verifier` surface.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use super::chain_of_custody::ChainOfCustody;
use super::policy_rule_id::PolicyRuleId;

/// Stable reference to an assumption ledger entry. Carried in
/// `CompatibilityProof.assumptions` and rendered in the UI's
/// AssumptionCard.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, TS, schemars::JsonSchema)]
#[ts(export)]
pub struct AssumptionRef {
    /// Id.
    pub id: String,
}

/// Where an assumption came from.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, TS, schemars::JsonSchema)]
#[ts(export)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AssumptionSource {
    /// SME explicitly accepted a default.
    SmeAccepted { rationale: String },
    /// LLM inferred a missing field from intake context.
    LlmInferred { confidence: String },
    /// Adapter classified as `LossyDeclared` was inserted.
    LossyAdapter { adapter_node_id: String },
    /// Profiler degraded (e.g. couldn't read the file).
    ProfilerDegraded { reason: String },
    /// Policy exception explicitly granted for a clinical/regulatory path.
    PolicyException { policy_id: String },
    /// Ontology mapping unresolved (LocalExtension proposed parents).
    OntologyMappingUnresolved { type_id: String },
    /// An atom was added without an explicit user override; the registry
    /// default for the affected port took effect.
    RegistryDefault {
        atom_id: crate::ids::AtomId,
        default_taken: String,
    },
    /// An ontology adapter node was inserted to bridge incompatible IRIs.
    OntologyAdapterInserted {
        /// From iri.
        from_iri: String,
        /// To iri.
        to_iri: String,
        /// Reason.
        reason: String,
    },
    /// A search-heuristic seed was selected (vs a deterministic archetype match).
    SeedHeuristic { strategy: String },
}

/// How an assumption was resolved (or not). Variants cover the
/// full lifecycle: `Accepted`, `Rejected`, `ResolvedByValidator`,
/// `Contradicted`, and `WaivedWithRisk`.
#[derive(
    Debug, Clone, Serialize, Deserialize, PartialEq, Eq, TS, Default, schemars::JsonSchema,
)]
#[ts(export)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AssumptionResolution {
    #[default]
    /// Unresolved variant.
    Unresolved,
    /// SME explicitly accepted the assumption — workflow can run as
    /// drafted.
    Accepted { rationale: String },
    /// SME rejected the assumption — composer must pivot or refuse.
    Rejected { rationale: String },
    /// Resolved via a downstream validation step; see `ValidatorRef`.
    ResolvedByValidator {
        /// Validator id.
        validator_id: String,
        /// Result ref.
        result_ref: String,
    },
    /// A later confirmation contradicted an earlier one.
    /// Both confirmations preserved in decision log; resolution returns
    /// to `Unresolved` for the next round.
    Contradicted {
        /// Prior confirmation id.
        prior_confirmation_id: String,
        /// Conflicting confirmation id.
        conflicting_confirmation_id: String,
    },
    /// Waived under the assumption-policy table with explicit
    /// risk acknowledgment. `policy_rule_id` names the row in
    /// `config/assumption-policy.yaml` that authorized the waiver.
    /// V3 §10.2 typed against the `policy_rules:` registry
    /// via [`PolicyRuleId`].
    WaivedWithRisk {
        /// Policy rule id.
        policy_rule_id: PolicyRuleId,
        /// Rationale.
        rationale: String,
        /// Credentials.
        credentials: Vec<String>,
    },
}

/// A single assumption entry in the ledger. Persisted via
/// `decision_log::DecisionRecord`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, TS, schemars::JsonSchema)]
#[ts(export)]
pub struct Assumption {
    /// Id.
    pub id: String,
    /// Statement.
    pub statement: String,
    /// Source.
    pub source: AssumptionSource,
    /// Task node ids this assumption affects. Always serialized
    /// (even when empty) so the TypeScript binding's `string[]` type
    /// matches the wire payload — older `skip_serializing_if =
    /// Vec::is_empty` caused the Composition tab to crash with
    /// `Cannot read properties of undefined (reading 'length')` on
    /// assumptions where the planner hadn't populated the field.
    #[serde(default)]
    pub affects_nodes: Vec<String>,
    /// Risk.
    pub risk: RiskClass,
    #[serde(default)]
    /// Resolution.
    pub resolution: AssumptionResolution,
    /// Chain-of-custody for the assumption when its
    /// statement carries suppressed content. `None` for ordinary
    /// non-suppressed assumptions. Older records without the field
    /// deserialize cleanly.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub chain_of_custody: Option<ChainOfCustody>,
}

/// A bag of assumptions. Materialized either inline on a
/// `WorkflowDag` or as a projection over `runtime/decisions.jsonl`.
#[derive(
    Debug, Clone, Serialize, Deserialize, PartialEq, Eq, TS, Default, schemars::JsonSchema,
)]
#[ts(export)]
pub struct AssumptionLedger {
    /// Entries.
    pub entries: Vec<Assumption>,
}

impl AssumptionLedger {
    /// Append an assumption entry to the ledger.
    pub fn push(&mut self, a: Assumption) {
        self.entries.push(a);
    }
}

/// Risk class — drives planner scoring and policy gates. Aligned
/// with design §3 + §15 risk language.
#[derive(
    Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, TS, Default, schemars::JsonSchema,
)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum RiskClass {
    #[default]
    /// Negligible variant.
    Negligible,
    /// Low variant.
    Low,
    /// Moderate variant.
    Moderate,
    /// High variant.
    High,
    /// Reserved for clinical/regulatory paths — treated as High plus
    /// additional policy gates.
    Clinical,
}

/// Reference to a validator implementation. Resolved by the harness
/// verify endpoint against `claim_extractor` + `claim_verifier` and
/// numeric/structural validators.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, TS, schemars::JsonSchema)]
#[ts(export)]
pub struct ValidatorRef {
    /// Stable validator id (`p_value_in_unit_interval`,
    /// `gene_id_in_annotation`, `coordinate_in_contig`).
    pub id: String,
    /// Optional version pin.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub version: Option<String>,
    /// Optional inline parameters (e.g. {"max_p": 1.0}). Schema is
    /// validator-specific; the composer treats this as opaque.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional, type = "Record<string, unknown>")]
    pub parameters: Option<serde_json::Value>,
}

/// Validation/promotion evidence attached to a `TaskNode`.
#[derive(
    Debug, Clone, Serialize, Deserialize, PartialEq, Eq, TS, Default, schemars::JsonSchema,
)]
#[ts(export)]
pub struct EvidenceSet {
    /// Validator runs that have already passed for this node.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub passed_validators: Vec<ValidatorRef>,
    /// Benchmarks completed (required for `BenchmarkValidatedNode` promotion).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub benchmarks: Vec<String>,
    /// Citations and reference data versions.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub citations: Vec<String>,
    /// Free-form notes for human review.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub notes: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn assumption_round_trip() {
        let a = Assumption {
            id: "a1".into(),
            statement: "Reads are unstranded".into(),
            source: AssumptionSource::LlmInferred {
                confidence: "low".into(),
            },
            affects_nodes: vec!["quantify_features".into()],
            risk: RiskClass::Moderate,
            resolution: AssumptionResolution::Unresolved,
            chain_of_custody: None,
        };
        let json = serde_json::to_string(&a).unwrap();
        let back: Assumption = serde_json::from_str(&json).unwrap();
        assert_eq!(a, back);
    }

    /// v3 P5 — `chain_of_custody` is omitted at serialize time when
    /// `None` so legacy records remain byte-identical.
    #[test]
    fn assumption_default_serialization_omits_custody() {
        let a = Assumption {
            id: "a1".into(),
            statement: "x".into(),
            source: AssumptionSource::SmeAccepted {
                rationale: "y".into(),
            },
            affects_nodes: vec![],
            risk: RiskClass::Low,
            resolution: AssumptionResolution::Unresolved,
            chain_of_custody: None,
        };
        let json = serde_json::to_string(&a).unwrap();
        assert!(!json.contains("chain_of_custody"), "got: {json}");
    }

    #[test]
    fn validator_ref_round_trip() {
        let v = ValidatorRef {
            id: "p_value_in_unit_interval".into(),
            version: Some("1".into()),
            parameters: Some(serde_json::json!({"strict": true})),
        };
        let json = serde_json::to_string(&v).unwrap();
        let back: ValidatorRef = serde_json::from_str(&json).unwrap();
        assert_eq!(v, back);
    }

    #[test]
    fn evidence_set_default_is_empty() {
        let e = EvidenceSet::default();
        let json = serde_json::to_string(&e).unwrap();
        // Default-empty serializes to {}.
        assert_eq!(json, "{}");
    }
}
