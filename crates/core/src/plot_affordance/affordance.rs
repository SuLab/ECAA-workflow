use serde::{Deserialize, Serialize};
use ts_rs::TS;

use super::primitive::GenericPrimitive;
use super::safety::PlotSafety;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS, schemars::JsonSchema)]
#[ts(export)]
#[serde(tag = "kind", rename_all = "snake_case")]
/// PlotAffordance discriminant.
pub enum PlotAffordance {
    /// Exact-match SemanticType in the closed catalog.
    /// Today's path; preferred when available.
    Registered {
        /// Figure ids.
        figure_ids: Vec<String>,
        /// Renderer module.
        renderer_module: String,
        /// Proof.
        proof: AffordanceProof,
    },
    /// Parent-term renderer reused via ontology subsumption.
    InheritedViaOntology {
        /// Parent term.
        parent_term: String,
        /// Figure ids.
        figure_ids: Vec<String>,
        /// Renderer module.
        renderer_module: String,
        /// Proof.
        proof: AffordanceProof,
    },
    /// Universal primitive selected by physical structure alone.
    StructuralFallback {
        /// Primitive.
        primitive: GenericPrimitive,
        /// Figure id.
        figure_id: String,
        /// Warning.
        warning: String,
        /// Proof.
        proof: AffordanceProof,
    },
    /// LLM-drafted renderer, sandboxed and validated.
    /// Reachable only after the generated-renderer alignment work ships.
    GeneratedSandboxed {
        /// Renderer module.
        renderer_module: String,
        /// Figure ids.
        figure_ids: Vec<String>,
        /// Review status.
        review_status: GeneratedReviewStatus,
        /// Proof.
        proof: AffordanceProof,
    },
    /// No automatic renderer; data artifact ships with a description
    /// of the SME-recommended visualization.
    Deferred {
        /// Data artifact relpath.
        data_artifact_relpath: String,
        /// Recommendation.
        recommendation: String,
        /// Sme check required.
        sme_check_required: bool,
        /// Proof.
        proof: AffordanceProof,
    },
}

impl PlotAffordance {
    /// Safety.
    pub fn safety(&self) -> PlotSafety {
        match self {
            Self::Registered { .. } => PlotSafety::Validated,
            Self::InheritedViaOntology { .. } => PlotSafety::InheritanceWarn,
            Self::StructuralFallback { .. } => PlotSafety::Generic,
            Self::GeneratedSandboxed { .. } => PlotSafety::Generated,
            Self::Deferred { .. } => PlotSafety::None,
        }
    }

    /// `true` for any variant whose figure must be marked
    /// `provisional: true` in WRROC export.
    pub fn is_provisional(&self) -> bool {
        !matches!(self, Self::Registered { .. })
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS, schemars::JsonSchema)]
#[ts(export)]
/// AffordanceProof data.
pub struct AffordanceProof {
    /// Output port semantic-type IRI or `LocalExtension` id the
    /// affordance was resolved against.
    pub source_semantic_type: String,
    /// Ordered ontology terms walked during resolution; empty for
    /// `Registered`/`StructuralFallback`/`Deferred`.
    pub ontology_walk: Vec<String>,
    /// Snapshot id of the registry the resolution consulted; required
    /// for replay determinism.
    pub registry_snapshot_id: String,
    /// Theme version (`theme.json` file digest) used at render time.
    pub theme_version: String,
    /// Human-readable rationale; surfaced in the UI badge.
    pub rationale: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS, schemars::JsonSchema)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
/// GeneratedReviewStatus discriminant.
pub enum GeneratedReviewStatus {
    /// Drafted variant.
    Drafted,
    /// SandboxValidated variant.
    SandboxValidated,
    /// HumanApproved variant.
    HumanApproved,
    /// Rejected variant.
    Rejected,
}
