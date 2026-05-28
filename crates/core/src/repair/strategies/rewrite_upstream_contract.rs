//! `rewrite_upstream_contract` — narrows the producer's declared port
//! contract so it matches the consumer's tighter constraints (e.g.
//! pinning a previously-unconstrained `annotation_version` to a
//! specific Ensembl release). High-risk because it rewrites the
//! producer's promise rather than papering over the mismatch.

use crate::composer_v4::PlanningContext;
use crate::repair::proposal::{
    DagModification, PortRole, RepairGap, RepairProposal, RepairRiskClass,
};
use crate::repair::strategy::{GapKind, RepairStrategy};
use crate::workflow_contracts::port::PortContract;
use crate::workflow_contracts::semantic_type::SemanticType;

/// RewriteUpstreamContractRepair data.
pub struct RewriteUpstreamContractRepair;

impl RepairStrategy for RewriteUpstreamContractRepair {
    fn id(&self) -> &'static str {
        "rewrite_upstream_contract"
    }

    fn applicable_to(&self) -> &'static [GapKind] {
        &[
            GapKind::AnnotationMismatch,
            GapKind::VersionMismatch,
            GapKind::ContractGap,
        ]
    }

    fn propose(&self, gap: &RepairGap, ctx: &PlanningContext) -> Option<RepairProposal> {
        let producer_node = gap.producer_node.clone()?;
        let producer_port = gap.producer_port.clone()?;
        // Pick the first facet mismatch and rewrite the producer's
        // contract to match the consumer's value. The strategy is
        // limited to the facets the v4 `PortContract` exposes — every
        // other facet name surfaces as a generic `rationale`-only
        // proposal that the SME must hand-resolve.
        let mismatch = gap.facet_mismatches.first()?;
        let mut new_contract = PortContract {
            name: producer_port.clone(),
            semantic_type: SemanticType::Opaque {
                description: format!(
                    "rewritten by repair strategy {} (gap {})",
                    self.id(),
                    gap.id
                ),
            },
            ..PortContract::default()
        };
        match mismatch.facet.as_str() {
            "annotation_version" => {
                new_contract.annotation_version = Some(mismatch.consumer_value.clone());
            }
            "genome_build" => {
                new_contract.genome_build = Some(mismatch.consumer_value.clone());
            }
            "organism" => {
                new_contract.organism = Some(mismatch.consumer_value.clone());
            }
            "units" => {
                new_contract.units = Some(mismatch.consumer_value.clone());
            }
            "normalization_state" => {
                new_contract.normalization_state = Some(mismatch.consumer_value.clone());
            }
            "coordinate_system" => {
                new_contract.coordinate_system = Some(mismatch.consumer_value.clone());
            }
            _ => return None,
        }
        let id = super::proposal_id("rewrite_upstream_contract", &gap.id, ctx);
        Some(RepairProposal {
            id,
            strategy_id: self.id().to_string(),
            gap_id: gap.id.clone(),
            modification: DagModification::RewriteContract {
                node: producer_node.clone(),
                new_contract,
                applies_to: PortRole::Output,
            },
            risk_class: RepairRiskClass::HighCredentialedReview,
            generated_assumptions: Vec::new(),
            required_credentials: vec!["bioinformatics_lead".into()],
            rationale: format!(
                "Tighten producer {}'s output contract on facet {} ({} → {}) to match \
                 consumer {} requirements (rewrites the producer's declared shape rather \
                 than inserting an adapter)",
                producer_node,
                mismatch.facet,
                mismatch.producer_value,
                mismatch.consumer_value,
                gap.consumer_node,
            ),
            ctx_snapshot_hash: super::ctx_snapshot_hash(ctx),
        })
    }
}
