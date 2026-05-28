//! Incompatibility reports — typed reasons an edge cannot hold.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// A single reason an edge is incompatible. Used in
/// `IncompatibilityReport.reasons` so a single report can carry
/// multiple independent failures (UI shows the full list).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, TS, schemars::JsonSchema)]
#[ts(export)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum IncompatibilityReason {
    /// The producer's semantic type cannot be subsumed under the
    /// consumer's. E.g. `data:0863` (BAM) → `data:3917` (count
    /// matrix) without an intermediate quantification step.
    SemanticTypeMismatch { producer: String, consumer: String },
    /// Producer and consumer disagree on a load-bearing facet
    /// with no defensible adapter path. Genome build mismatch is
    /// the canonical example.
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
    /// Privacy class would need to widen across the edge (PHI →
    /// public, restricted → internal). Refused by the policy
    /// gate with a typed reason.
    PrivacyClassWidening { producer: String, consumer: String },
    /// Cardinality cannot be reconciled (e.g. consumer wants
    /// `One`, producer is `Many` and no scatter/gather declared).
    CardinalityMismatch { producer: String, consumer: String },
    /// An active
    /// `PolicyContext` bundle refused this edge. `bundle_id` and
    /// `check_kind` identify which policy fired so the UI can route
    /// to the right policy-exception card.
    PolicyViolation {
        /// Bundle id.
        bundle_id: String,
        /// Check kind.
        check_kind: String,
        /// Statement.
        statement: String,
    },
    /// Generic free-text fallback for situations that don't fit
    /// the typed reasons above.
    Other { statement: String },
}

/// A bundle of incompatibility reasons. Returned when the
/// compatibility engine concludes the edge cannot hold.
#[derive(
    Debug, Clone, Serialize, Deserialize, PartialEq, Eq, TS, Default, schemars::JsonSchema,
)]
#[ts(export)]
pub struct IncompatibilityReport {
    /// Reasons.
    pub reasons: Vec<IncompatibilityReason>,
    /// Optional rationale for human reading.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub rationale: Option<String>,
}

impl IncompatibilityReport {
    /// New.
    pub fn new(reasons: Vec<IncompatibilityReason>) -> Self {
        Self {
            reasons,
            rationale: None,
        }
    }

    /// With rationale.
    pub fn with_rationale(mut self, r: impl Into<String>) -> Self {
        self.rationale = Some(r.into());
        self
    }
}
