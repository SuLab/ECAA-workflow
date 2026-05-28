//! `insert_gzip_decompression` — inserts a gzip-decompression converter
//! when producer's output is gzip-compressed but consumer wants the
//! uncompressed form. Lossless mechanical conversion → `LowAutoAttempt`.

use crate::composer_v4::PlanningContext;
use crate::repair::proposal::{
    DagModification, PortRef, RepairGap, RepairProposal, RepairRiskClass,
};
use crate::repair::strategy::{GapKind, RepairStrategy};
use crate::workflow_contracts::task_node::TaskNode;

/// InsertGzipDecompressionConverterRepair data.
pub struct InsertGzipDecompressionConverterRepair;

impl RepairStrategy for InsertGzipDecompressionConverterRepair {
    fn id(&self) -> &'static str {
        "insert_gzip_decompression"
    }

    fn applicable_to(&self) -> &'static [GapKind] {
        &[GapKind::ContractGap, GapKind::AmbiguousMatch]
    }

    fn propose(&self, gap: &RepairGap, ctx: &PlanningContext) -> Option<RepairProposal> {
        // Strategy fires only when the gap's facet mismatch names the
        // compression facet (and producer is `gzip` / consumer is the
        // bare format). Repair is purely structural — splice a
        // decompression converter onto the offending edge.
        let mismatch = gap
            .facet_mismatches
            .iter()
            .find(|f| f.facet == "compression" || f.facet == "physical_format")?;
        let producer_value = mismatch.producer_value.to_lowercase();
        if !producer_value.contains("gz") && !producer_value.contains("gzip") {
            return None;
        }
        let producer_node = gap.producer_node.clone()?;
        let producer_port = gap.producer_port.clone()?;
        let converter = TaskNode::skeleton(
            format!("gunzip_{}", gap.consumer_node),
            "Decompress gzip-compressed input for downstream consumer.",
        );
        let id = super::proposal_id("insert_gzip_decompression", &gap.id, ctx);
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
                "Insert gzip decompression converter to bridge compression mismatch \
                 on edge into {}:{} (producer reports {}, consumer requires uncompressed)",
                gap.consumer_node, gap.consumer_port, mismatch.producer_value,
            ),
            ctx_snapshot_hash: super::ctx_snapshot_hash(ctx),
        })
    }
}
