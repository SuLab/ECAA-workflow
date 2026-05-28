//! `request_genome_build_metadata` — surfaces an
//! `UnblockPath::SupplyMissingMetadata { field: "genome_build",.. }`
//! when the consumer requires a genome build annotation the upstream
//! producer can't pin. No DAG mutation; user-gated.

use crate::composer_v4::PlanningContext;
use crate::repair::proposal::{DagModification, RepairGap, RepairProposal, RepairRiskClass};
use crate::repair::strategy::{GapKind, RepairStrategy};

/// RequestGenomeBuildMetadataRepair data.
pub struct RequestGenomeBuildMetadataRepair;

impl RepairStrategy for RequestGenomeBuildMetadataRepair {
    fn id(&self) -> &'static str {
        "request_genome_build_metadata"
    }

    fn applicable_to(&self) -> &'static [GapKind] {
        &[GapKind::MissingMetadata, GapKind::ContractGap]
    }

    fn propose(&self, gap: &RepairGap, ctx: &PlanningContext) -> Option<RepairProposal> {
        // Only fires when the gap is a missing-metadata case — the
        // `insert_liftover` strategy already handles
        // `ReferenceMismatch` when both producer + consumer values are
        // set. This strategy fills the *missing* case: producer is
        // genome-build-unknown OR consumer doesn't yet have a target
        // build pinned.
        let facet_match = gap.facet_mismatches.iter().any(|f| {
            f.facet == "genome_build"
                && (f.producer_value.is_empty() || f.consumer_value.is_empty())
        });
        let statement_match = gap.statement.to_lowercase().contains("genome build");
        if !facet_match && !statement_match {
            return None;
        }
        let id = super::proposal_id("request_genome_build_metadata", &gap.id, ctx);
        Some(RepairProposal {
            id,
            strategy_id: self.id().to_string(),
            gap_id: gap.id.clone(),
            modification: DagModification::RequestMissingMetadata {
                field: "genome_build".into(),
                applies_to_node: gap.consumer_node.clone(),
            },
            risk_class: RepairRiskClass::MediumUserGated,
            generated_assumptions: Vec::new(),
            required_credentials: Vec::new(),
            rationale: format!(
                "Consumer {} requires a genome_build annotation that the upstream producer \
                 did not declare. Supply the target build (e.g. GRCh38, GRCm39).",
                gap.consumer_node
            ),
            ctx_snapshot_hash: super::ctx_snapshot_hash(ctx),
        })
    }
}
