//! Audit-proof invariant types — A sub-graph wire shape.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS, schemars::JsonSchema)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum InvariantId {
    ClaimCompleteness,
    DecisionJustification,
    EvidenceCoverage,
    EquivalenceFailure,
    CrossGraphIntegrity,
    SubstrateValidity,
}

impl InvariantId {
    pub const ALL: [InvariantId; 6] = [
        InvariantId::ClaimCompleteness,
        InvariantId::DecisionJustification,
        InvariantId::EvidenceCoverage,
        InvariantId::EquivalenceFailure,
        InvariantId::CrossGraphIntegrity,
        InvariantId::SubstrateValidity,
    ];
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS, schemars::JsonSchema)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum InvariantStatus {
    Pass,
    Warn,
    Fail,
    Unverified,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, schemars::JsonSchema)]
#[ts(export)]
pub struct InvariantVerdict {
    pub id: InvariantId,
    pub status: InvariantStatus,
    /// Human-readable detail when status != Pass.
    pub detail: Option<String>,
    /// Items inspected (e.g., number of claims, decisions, edges).
    pub n_inspected: usize,
    /// Items violating the invariant.
    pub n_violations: usize,
}

/// Evaluator provenance — informative per invariants.md §"audit-proof-report.json shape".
#[derive(Debug, Clone, Serialize, Deserialize, TS, schemars::JsonSchema)]
#[ts(export)]
pub struct EvaluatorInfo {
    /// Implementation identifier.
    pub r#impl: String,
    /// Evaluator implementation version.
    pub version: String,
    /// Warn/fail policy: "warn-only" | "strict" (absent ⇒ normative defaults).
    pub policy: String,
}

impl EvaluatorInfo {
    /// The reference evaluator shipped with this crate.
    pub fn reference() -> Self {
        Self {
            r#impl: "ecaa-workflow-audit-proof".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            policy: "warn-only".to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, schemars::JsonSchema)]
#[ts(export)]
pub struct AuditProofReport {
    /// On-disk schema version of this report shape.
    pub schema_version: String,
    /// ECAA spec version this package conforms to (§9.2). Required.
    pub ecaa_version: String,
    /// Minimum reader version required to consume the package (§9.2). Required.
    pub min_reader_version: String,
    /// Maximum reader version, when the emitter pins an upper bound (§9.2).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub max_reader_version: Option<String>,
    /// IRI (or relative path) of the package's ro-crate-metadata.json.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub package_iri: Option<String>,
    /// RFC-3339 timestamp the report was produced. Excluded from the
    /// BagIt manifest so it does not break byte-reproducibility.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub evaluated_at: Option<String>,
    /// Evaluator provenance (informative).
    pub evaluator: EvaluatorInfo,
    /// Per-invariant verdicts.
    pub verdicts: Vec<InvariantVerdict>,
}

impl AuditProofReport {
    pub fn empty() -> Self {
        Self {
            schema_version: "0.1".to_string(),
            ecaa_version: crate::consts::ECAA_VERSION.to_string(),
            min_reader_version: crate::consts::MIN_READER_VERSION.to_string(),
            max_reader_version: None,
            package_iri: None,
            evaluated_at: None,
            evaluator: EvaluatorInfo::reference(),
            verdicts: InvariantId::ALL
                .iter()
                .map(|&id| InvariantVerdict {
                    id,
                    status: InvariantStatus::Unverified,
                    detail: None,
                    n_inspected: 0,
                    n_violations: 0,
                })
                .collect(),
        }
    }
}
