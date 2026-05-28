//! `substitute_compatible_producer` — swaps the current producer for
//! a different registry atom whose declared output unifies cleanly
//! with the consumer's input. Used when the planner picked a producer
//! that almost matches but a sibling atom matches better. Medium-risk
//! (changes the workflow's scientific shape, not just its plumbing).

use crate::composer_v4::PlanningContext;
use crate::repair::proposal::{DagModification, RepairGap, RepairProposal, RepairRiskClass};
use crate::repair::strategy::{GapKind, RepairStrategy};
use crate::workflow_contracts::task_node::TaskNode;

/// SubstituteCompatibleProducerRepair data.
pub struct SubstituteCompatibleProducerRepair;

impl RepairStrategy for SubstituteCompatibleProducerRepair {
    fn id(&self) -> &'static str {
        "substitute_compatible_producer"
    }

    fn applicable_to(&self) -> &'static [GapKind] {
        &[GapKind::AmbiguousMatch, GapKind::ContractGap]
    }

    fn propose(&self, gap: &RepairGap, ctx: &PlanningContext) -> Option<RepairProposal> {
        let producer_node = gap.producer_node.clone()?;
        // Pure structural strategy — no facet inspection; the planner
        // surfaces an `AmbiguousMatch` gap when multiple candidate
        // producers exist. The replacement TaskNode is a skeleton —
        // the accept endpoint resolves it to a concrete atom by
        // re-running the search with `exclude(producer_node)`.
        let id = super::proposal_id("substitute_compatible_producer", &gap.id, ctx);
        let replacement_id = format!("alt_{}", producer_node);
        let replacement = TaskNode::skeleton(
            replacement_id.clone(),
            "Alternative producer selected by the substitute_compatible_producer repair strategy.",
        );
        Some(RepairProposal {
            id,
            strategy_id: self.id().to_string(),
            gap_id: gap.id.clone(),
            modification: DagModification::SubstituteProducer {
                remove: producer_node.clone(),
                add: replacement,
            },
            risk_class: RepairRiskClass::MediumUserGated,
            generated_assumptions: Vec::new(),
            required_credentials: Vec::new(),
            rationale: format!(
                "Substitute producer {} with an alternative atom selected by re-running search \
                 excluding the current pick (consumer {} still unsatisfied on port {})",
                producer_node, gap.consumer_node, gap.consumer_port,
            ),
            ctx_snapshot_hash: super::ctx_snapshot_hash(ctx),
        })
    }
}
