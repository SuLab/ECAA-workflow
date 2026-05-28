//! `RepairStrategy` trait + [`GapKind`] discriminator.
//!
//! Each strategy implements the trait, declares
//! which [`GapKind`]s it can address, and produces an opaque
//! [`super::proposal::RepairProposal`] when its preconditions are met.
//! The registry filters strategies by `applicable_to()` before dispatch
//! so strategies never receive gaps outside their declared scope.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use super::proposal::{RepairGap, RepairProposal};
use crate::composer_v4::PlanningContext;

/// Trait implemented by every repair strategy. Stateless — strategies
/// hold no per-call state; the registry constructs one of each at
/// startup via [`super::registry::RepairRegistry::with_builtin`].
pub trait RepairStrategy: Send + Sync {
    /// Stable identifier persisted in `RepairProposal::strategy_id` and
    /// Referenced from `UnblockPath::AttemptRepair { strategy_id,.. }`.
    /// MUST be a snake_case, registry-unique string.
    fn id(&self) -> &'static str;

    /// Which [`GapKind`]s this strategy can address. The registry uses
    /// this for dispatch routing; a strategy is invoked only when at
    /// least one of its declared kinds matches the incoming gap.
    fn applicable_to(&self) -> &'static [GapKind];

    /// Try to build a [`RepairProposal`] for `gap` under `ctx`.
    /// Returns `None` when the gap doesn't fit this strategy's
    /// specific repair pattern (e.g. wrong facet, missing context
    /// field). MUST be deterministic — same gap + same ctx → same
    /// proposal.
    fn propose(&self, gap: &RepairGap, ctx: &PlanningContext) -> Option<RepairProposal>;
}

/// Coarse-grained classification of an unsatisfied gap. The repair
/// registry routes strategies by matching this discriminator. Strategies
/// then refine via gap.facet_mismatches / structural inspection.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS, schemars::JsonSchema,
)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum GapKind {
    /// No producer atom in the registry produces the consumer's input
    /// port semantic type.
    MissingProducer,
    /// Consumer's port carries an optional metadata field
    /// (`organism`, `genome_build`, `strandedness`, …) that the producer
    /// can't populate without more SME-supplied context.
    MissingMetadata,
    /// Multiple candidate producers satisfy the port shape but none is
    /// clearly preferred — disambiguation needed.
    AmbiguousMatch,
    /// Producer + consumer ports differ on a version-pinned facet
    /// (semantic_type ontology_version, structural_schema version).
    VersionMismatch,
    /// Producer + consumer differ on `genome_build` — needs liftover or
    /// re-alignment.
    ReferenceMismatch,
    /// Producer + consumer differ on `annotation_version` (Ensembl
    /// release, GENCODE version).
    AnnotationMismatch,
    /// Generic contract gap — typed gap reported by the search but
    /// without a more-specific kind classification.
    ContractGap,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gap_kind_equality_holds() {
        assert_eq!(GapKind::MissingProducer, GapKind::MissingProducer);
        assert_ne!(GapKind::MissingProducer, GapKind::MissingMetadata);
    }
}
