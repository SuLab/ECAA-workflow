//! Builtin repair-strategy registry. The registry holds an immutable
//! `Vec<Arc<dyn RepairStrategy>>` constructed via
//! [`RepairRegistry::with_builtin`] at startup; [`RepairRegistry::propose`]
//! filters by `applicable_to()`, invokes each surviving strategy, and
//! returns the resulting proposals sorted deterministically by
//! `(risk_class, strategy_id, ctx_snapshot_hash)`.
//!
//! F20: the registry itself never mutates the DAG. The planner is the
//! only call site that consults `risk_class` against
//! `PlanningContext::auto_attempt_risk_threshold`.

use std::sync::Arc;

use super::proposal::{RepairGap, RepairProposal};
use super::strategies::*;
use super::strategy::RepairStrategy;
use crate::composer_v4::PlanningContext;

/// Closed registry of repair strategies. Constructed once at startup.
pub struct RepairRegistry {
    strategies: Vec<Arc<dyn RepairStrategy>>,
}

impl std::fmt::Debug for RepairRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RepairRegistry")
            .field("strategy_count", &self.strategies.len())
            .field(
                "strategy_ids",
                &self.strategies.iter().map(|s| s.id()).collect::<Vec<_>>(),
            )
            .finish()
    }
}

impl RepairRegistry {
    /// Construct the eight-strategy builtin registry. Returned as an
    /// `Arc` so the planning context (cloned per composition attempt)
    /// can share it cheaply.
    pub fn with_builtin() -> Arc<Self> {
        let strategies: Vec<Arc<dyn RepairStrategy>> = vec![
            Arc::new(InsertGzipDecompressionConverterRepair),
            Arc::new(InsertSortIndexConverterRepair),
            Arc::new(InsertLiftoverRepair),
            Arc::new(RequestStrandednessMetadataRepair),
            Arc::new(RequestGenomeBuildMetadataRepair),
            Arc::new(SubstituteCompatibleProducerRepair),
            Arc::new(QueryRegistryForMatchingProducerRepair),
            Arc::new(RewriteUpstreamContractRepair),
        ];
        Arc::new(Self { strategies })
    }

    /// Construct an empty registry — useful for tests that want to
    /// assert the planner's "no proposals" path.
    pub fn empty() -> Arc<Self> {
        Arc::new(Self {
            strategies: Vec::new(),
        })
    }

    /// Strategy count (mostly for telemetry + the F20 property test).
    pub fn len(&self) -> usize {
        self.strategies.len()
    }

    /// `true` when no strategies are registered.
    pub fn is_empty(&self) -> bool {
        self.strategies.is_empty()
    }

    /// Run every applicable strategy against `gap` under `ctx` and
    /// return the proposals sorted by
    /// `(risk_class, strategy_id, ctx_snapshot_hash)`. Lower
    /// `risk_class` sorts first (so `LowAutoAttempt` proposals appear
    /// ahead of `MediumUserGated` / `HighCredentialedReview`).
    pub fn propose(&self, gap: &RepairGap, ctx: &PlanningContext) -> Vec<RepairProposal> {
        let mut out: Vec<RepairProposal> = self
            .strategies
            .iter()
            .filter(|s| s.applicable_to().contains(&gap.kind))
            .filter_map(|s| s.propose(gap, ctx))
            .collect();
        out.sort_by(|a, b| {
            (a.risk_class as u8)
                .cmp(&(b.risk_class as u8))
                .then(a.strategy_id.cmp(&b.strategy_id))
                .then(a.ctx_snapshot_hash.cmp(&b.ctx_snapshot_hash))
        });
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repair::proposal::{FacetMismatch, RepairRiskClass};
    use crate::repair::strategy::GapKind;
    use crate::workflow_contracts::workflow_intent::WorkflowIntent;

    fn sample_ctx() -> PlanningContext {
        PlanningContext::new(WorkflowIntent {
            id: "sess".into(),
            schema_version: semver::Version::new(1, 0, 0),
            goal: "test".into(),
            modality: Some("bulk_rnaseq".into()),
            ..Default::default()
        })
    }

    fn liftover_gap() -> RepairGap {
        RepairGap {
            id: "g_lift".into(),
            statement: "genome_build mismatch".into(),
            kind: GapKind::ReferenceMismatch,
            consumer_node: "downstream".into(),
            consumer_port: "in_bam".into(),
            producer_node: Some("upstream".into()),
            producer_port: Some("out_bam".into()),
            facet_mismatches: vec![FacetMismatch {
                facet: "genome_build".into(),
                producer_value: "GRCh37".into(),
                consumer_value: "GRCh38".into(),
            }],
        }
    }

    #[test]
    fn builtin_registry_loads_eight_strategies() {
        let r = RepairRegistry::with_builtin();
        assert_eq!(r.len(), 8);
    }

    #[test]
    fn empty_registry_returns_no_proposals() {
        let r = RepairRegistry::empty();
        let ctx = sample_ctx();
        assert!(r.propose(&liftover_gap(), &ctx).is_empty());
    }

    #[test]
    fn reference_mismatch_gap_triggers_liftover_only() {
        let r = RepairRegistry::with_builtin();
        let ctx = sample_ctx();
        let props = r.propose(&liftover_gap(), &ctx);
        assert!(!props.is_empty(), "expected at least one proposal");
        assert!(
            props.iter().any(|p| p.strategy_id == "insert_liftover"),
            "expected insert_liftover among proposals: {:?}",
            props.iter().map(|p| &p.strategy_id).collect::<Vec<_>>()
        );
        // Every proposal returned must declare the correct risk class
        // (liftover is HighCredentialedReview).
        for p in &props {
            if p.strategy_id == "insert_liftover" {
                assert_eq!(p.risk_class, RepairRiskClass::HighCredentialedReview);
            }
        }
    }

    #[test]
    fn registry_sort_is_deterministic_across_calls() {
        let r = RepairRegistry::with_builtin();
        let ctx = sample_ctx();
        let a = r.propose(&liftover_gap(), &ctx);
        let b = r.propose(&liftover_gap(), &ctx);
        let ids_a: Vec<_> = a.iter().map(|p| p.id.clone()).collect();
        let ids_b: Vec<_> = b.iter().map(|p| p.id.clone()).collect();
        assert_eq!(ids_a, ids_b);
    }

    #[test]
    fn registry_sort_orders_low_before_high_risk() {
        let r = RepairRegistry::with_builtin();
        let ctx = sample_ctx();
        // Use an AmbiguousMatch gap with a gzip facet — fires
        // both the LowAutoAttempt `insert_gzip_decompression` and the
        // MediumUserGated `substitute_compatible_producer`. Sort
        // must put Low before Medium.
        let gap = RepairGap {
            id: "g_amb".into(),
            statement: "gzip".into(),
            kind: GapKind::AmbiguousMatch,
            consumer_node: "n2".into(),
            consumer_port: "p2".into(),
            producer_node: Some("n1".into()),
            producer_port: Some("p1".into()),
            facet_mismatches: vec![FacetMismatch {
                facet: "compression".into(),
                producer_value: "gzip".into(),
                consumer_value: "uncompressed".into(),
            }],
        };
        let props = r.propose(&gap, &ctx);
        assert!(props.len() >= 2);
        // First proposal must be the lowest-risk one.
        for w in props.windows(2) {
            assert!(w[0].risk_class as u8 <= w[1].risk_class as u8);
        }
    }
}
