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

#[derive(Debug, Clone, Serialize, Deserialize, TS, schemars::JsonSchema)]
#[ts(export)]
pub struct AuditProofReport {
    pub schema_version: String,
    pub verdicts: Vec<InvariantVerdict>,
}

impl AuditProofReport {
    pub fn empty() -> Self {
        Self {
            schema_version: "0.1".to_string(),
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
