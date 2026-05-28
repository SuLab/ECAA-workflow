//! `query_registry_for_match` — issues a registry lookup for a
//! producer that should match the consumer's required semantic type.
//! Fires when `MissingProducer` is reported despite the gap's
//! semantic_type being well-known. Medium-risk (uses the existing
//! `external_registry` index — accept dispatches the query, results
//! re-enter the planner).

use crate::composer_v4::PlanningContext;
use crate::repair::proposal::{
    DagModification, RegistryQuery, RepairGap, RepairProposal, RepairRiskClass,
};
use crate::repair::strategy::{GapKind, RepairStrategy};

/// QueryRegistryForMatchingProducerRepair data.
pub struct QueryRegistryForMatchingProducerRepair;

impl RepairStrategy for QueryRegistryForMatchingProducerRepair {
    fn id(&self) -> &'static str {
        "query_registry_for_match"
    }

    fn applicable_to(&self) -> &'static [GapKind] {
        &[GapKind::MissingProducer]
    }

    fn propose(&self, gap: &RepairGap, ctx: &PlanningContext) -> Option<RepairProposal> {
        // Strategy needs at least a semantic-type hint to query for.
        // Prefer the facet mismatch with facet="semantic_type" when
        // present; fall back to the consumer port name as a coarse
        // signal otherwise.
        let semantic_type = gap
            .facet_mismatches
            .iter()
            .find(|f| f.facet == "semantic_type")
            .map(|f| f.consumer_value.clone())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| gap.consumer_port.clone());
        if semantic_type.is_empty() {
            return None;
        }
        let id = super::proposal_id("query_registry_for_match", &gap.id, ctx);
        Some(RepairProposal {
            id,
            strategy_id: self.id().to_string(),
            gap_id: gap.id.clone(),
            modification: DagModification::QueryRegistry {
                criteria: RegistryQuery {
                    semantic_type: semantic_type.clone(),
                    modality: ctx.intent.modality.clone(),
                },
                target_gap: gap.id.clone(),
            },
            risk_class: RepairRiskClass::MediumUserGated,
            generated_assumptions: Vec::new(),
            required_credentials: Vec::new(),
            rationale: format!(
                "Query external registry for a producer matching semantic_type {} \
                 (modality={:?}); accept re-runs planner with the registry result",
                semantic_type, ctx.intent.modality,
            ),
            ctx_snapshot_hash: super::ctx_snapshot_hash(ctx),
        })
    }
}
