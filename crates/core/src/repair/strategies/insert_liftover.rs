//! `insert_liftover` — splices a genome-build liftover adapter onto a
//! reference-mismatched edge. Risky (introduces coordinate uncertainty)
//! → `HighCredentialedReview`.

use crate::composer_v4::PlanningContext;
use crate::repair::proposal::{
    DagModification, EdgeRef, PortRef, RepairGap, RepairProposal, RepairRiskClass,
};
use crate::repair::strategy::{GapKind, RepairStrategy};
use crate::workflow_contracts::evidence::{
    Assumption, AssumptionResolution, AssumptionSource, RiskClass,
};

/// InsertLiftoverRepair data.
pub struct InsertLiftoverRepair;

impl RepairStrategy for InsertLiftoverRepair {
    fn id(&self) -> &'static str {
        "insert_liftover"
    }

    fn applicable_to(&self) -> &'static [GapKind] {
        &[GapKind::ReferenceMismatch]
    }

    fn propose(&self, gap: &RepairGap, ctx: &PlanningContext) -> Option<RepairProposal> {
        let mismatch = gap
            .facet_mismatches
            .iter()
            .find(|f| f.facet == "genome_build")?;
        let producer_node = gap.producer_node.clone()?;
        let producer_port = gap.producer_port.clone()?;
        let id = super::proposal_id("insert_liftover", &gap.id, ctx);
        Some(RepairProposal {
            id: id.clone(),
            strategy_id: self.id().to_string(),
            gap_id: gap.id.clone(),
            modification: DagModification::InsertLiftover {
                from_build: mismatch.producer_value.clone(),
                to_build: mismatch.consumer_value.clone(),
                target_edge: EdgeRef {
                    from: PortRef {
                        node_id: producer_node,
                        port_name: producer_port,
                    },
                    to: PortRef {
                        node_id: gap.consumer_node.clone(),
                        port_name: gap.consumer_port.clone(),
                    },
                },
            },
            risk_class: RepairRiskClass::HighCredentialedReview,
            generated_assumptions: vec![Assumption {
                id: format!("liftover_uncertainty:{id}"),
                statement: format!(
                    "liftover from {} to {} introduces coordinate uncertainty; downstream \
                     interpretation must account for failed-lift / multi-mapped regions",
                    mismatch.producer_value, mismatch.consumer_value
                ),
                source: AssumptionSource::LossyAdapter {
                    adapter_node_id: "liftover_inserted_by_repair".into(),
                },
                affects_nodes: vec![gap.consumer_node.clone()],
                risk: RiskClass::High,
                resolution: AssumptionResolution::Unresolved,
                chain_of_custody: None,
            }],
            required_credentials: vec!["bioinformatics_lead".into()],
            rationale: format!(
                "Insert genome liftover from {} to {} to bridge reference mismatch on edge into {}:{}",
                mismatch.producer_value, mismatch.consumer_value, gap.consumer_node, gap.consumer_port
            ),
            ctx_snapshot_hash: super::ctx_snapshot_hash(ctx),
        })
    }
}
