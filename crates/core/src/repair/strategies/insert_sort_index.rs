//! `insert_sort_index` — inserts a `samtools sort` / `samtools index`
//! converter when consumer requires sorted+indexed BAM but producer
//! reports unsorted output. Lossless mechanical → `LowAutoAttempt`.

use crate::composer_v4::PlanningContext;
use crate::repair::proposal::{
    DagModification, PortRef, RepairGap, RepairProposal, RepairRiskClass,
};
use crate::repair::strategy::{GapKind, RepairStrategy};
use crate::workflow_contracts::task_node::TaskNode;

/// InsertSortIndexConverterRepair data.
pub struct InsertSortIndexConverterRepair;

impl RepairStrategy for InsertSortIndexConverterRepair {
    fn id(&self) -> &'static str {
        "insert_sort_index"
    }

    fn applicable_to(&self) -> &'static [GapKind] {
        &[GapKind::ContractGap, GapKind::AmbiguousMatch]
    }

    fn propose(&self, gap: &RepairGap, ctx: &PlanningContext) -> Option<RepairProposal> {
        let mismatch = gap.facet_mismatches.iter().find(|f| {
            matches!(
                f.facet.as_str(),
                "sort_order" | "indexed" | "coordinate_sorted"
            )
        })?;
        let producer_node = gap.producer_node.clone()?;
        let producer_port = gap.producer_port.clone()?;
        let converter = TaskNode::skeleton(
            format!("sort_index_{}", gap.consumer_node),
            "Coordinate-sort and index the producer's BAM so the downstream consumer can random-access it.",
        );
        let id = super::proposal_id("insert_sort_index", &gap.id, ctx);
        Some(RepairProposal {
            id,
            strategy_id: self.id().to_string(),
            gap_id: gap.id.clone(),
            modification: DagModification::InsertConverter {
                converter_node: converter,
                source_port: PortRef {
                    node_id: producer_node,
                    port_name: producer_port,
                },
                sink_port: PortRef {
                    node_id: gap.consumer_node.clone(),
                    port_name: gap.consumer_port.clone(),
                },
            },
            risk_class: RepairRiskClass::LowAutoAttempt,
            generated_assumptions: Vec::new(),
            required_credentials: Vec::new(),
            rationale: format!(
                "Insert sort+index converter on the edge into {}:{} (producer reports {}={}, \
                 consumer requires {}={})",
                gap.consumer_node,
                gap.consumer_port,
                mismatch.facet,
                mismatch.producer_value,
                mismatch.facet,
                mismatch.consumer_value,
            ),
            ctx_snapshot_hash: super::ctx_snapshot_hash(ctx),
        })
    }
}
