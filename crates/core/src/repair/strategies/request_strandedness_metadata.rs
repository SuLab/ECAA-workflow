//! `request_strandedness_metadata` — surfaces an
//! `UnblockPath::SupplyMissingMetadata { field: "strandedness",.. }`
//! when consumer requires a strandedness annotation the producer can't
//! declare. No DAG mutation; user-gated.

use crate::composer_v4::PlanningContext;
use crate::repair::proposal::{DagModification, RepairGap, RepairProposal, RepairRiskClass};
use crate::repair::strategy::{GapKind, RepairStrategy};

/// RequestStrandednessMetadataRepair data.
pub struct RequestStrandednessMetadataRepair;

impl RepairStrategy for RequestStrandednessMetadataRepair {
    fn id(&self) -> &'static str {
        "request_strandedness_metadata"
    }

    fn applicable_to(&self) -> &'static [GapKind] {
        &[GapKind::MissingMetadata, GapKind::ContractGap]
    }

    fn propose(&self, gap: &RepairGap, ctx: &PlanningContext) -> Option<RepairProposal> {
        // Two firing conditions: explicit `strandedness` facet mismatch
        // OR a `MissingMetadata` gap whose statement mentions
        // strandedness.
        let facet_match = gap
            .facet_mismatches
            .iter()
            .any(|f| f.facet == "strandedness");
        let statement_match = gap.statement.to_lowercase().contains("stranded");
        if !facet_match && !statement_match {
            return None;
        }
        let id = super::proposal_id("request_strandedness_metadata", &gap.id, ctx);
        Some(RepairProposal {
            id,
            strategy_id: self.id().to_string(),
            gap_id: gap.id.clone(),
            modification: DagModification::RequestMissingMetadata {
                field: "strandedness".into(),
                applies_to_node: gap.consumer_node.clone(),
            },
            risk_class: RepairRiskClass::MediumUserGated,
            generated_assumptions: Vec::new(),
            required_credentials: Vec::new(),
            rationale: format!(
                "Consumer {} requires a strandedness annotation that the producer \
                 cannot derive without SME guidance",
                gap.consumer_node
            ),
            ctx_snapshot_hash: super::ctx_snapshot_hash(ctx),
        })
    }
}
