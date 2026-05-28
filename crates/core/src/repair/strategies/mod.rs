//! Builtin repair strategies. Each submodule is a single
//! `impl RepairStrategy` for a focused repair pattern. The
//! [`super::registry::RepairRegistry::with_builtin`] constructor
//! instantiates one of each at startup.
//!
//! All eight strategies share two deterministic-id helpers:
//!
//! - [`proposal_id`] — hashes (strategy_id, gap_id, ctx_snapshot_hash)
//!   into a stable proposal id so re-running the planner on the same
//!   inputs produces the same id (F20 determinism contract).
//! - [`ctx_snapshot_hash`] — hashes the planning-context fields that
//!   matter for repair (intent id + modality + atom-snapshot id) into
//!   a fingerprint the accept endpoint can use to refuse stale
//!   proposals.

use sha2::{Digest, Sha256};

use crate::composer_v4::PlanningContext;

pub mod insert_gzip_decompression;
pub mod insert_liftover;
pub mod insert_sort_index;
pub mod query_registry_for_match;
pub mod request_genome_build_metadata;
pub mod request_strandedness_metadata;
pub mod rewrite_upstream_contract;
pub mod substitute_compatible_producer;

pub use insert_gzip_decompression::InsertGzipDecompressionConverterRepair;
pub use insert_liftover::InsertLiftoverRepair;
pub use insert_sort_index::InsertSortIndexConverterRepair;
pub use query_registry_for_match::QueryRegistryForMatchingProducerRepair;
pub use request_genome_build_metadata::RequestGenomeBuildMetadataRepair;
pub use request_strandedness_metadata::RequestStrandednessMetadataRepair;
pub use rewrite_upstream_contract::RewriteUpstreamContractRepair;
pub use substitute_compatible_producer::SubstituteCompatibleProducerRepair;

/// Deterministic proposal id. Combining strategy_id + gap_id + ctx
/// snapshot hash gives every (strategy, gap, ctx) triple a stable
/// identifier across runs.
pub(crate) fn proposal_id(strategy: &str, gap_id: &str, ctx: &PlanningContext) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"repair:");
    hasher.update(strategy.as_bytes());
    hasher.update(b":");
    hasher.update(gap_id.as_bytes());
    hasher.update(b":");
    hasher.update(ctx_snapshot_hash(ctx).as_bytes());
    let h = hasher.finalize();
    let hex: String = h.iter().take(8).map(|b| format!("{b:02x}")).collect();
    format!("repair:{strategy}:{hex}")
}

/// Hash of the planning-context fields a repair proposal depends on.
/// Surfaced on every proposal as `ctx_snapshot_hash` so the accept
/// endpoint can reject proposals computed against a context that has
/// since changed (intake amended, modality changed, atom snapshot
/// rotated).
pub(crate) fn ctx_snapshot_hash(ctx: &PlanningContext) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"ctx:");
    hasher.update(ctx.intent.id.as_bytes());
    hasher.update(b":");
    hasher.update(ctx.intent.modality.as_deref().unwrap_or("").as_bytes());
    hasher.update(b":");
    hasher.update(ctx.atom_snapshot_id.as_deref().unwrap_or("").as_bytes());
    hasher.update(b":");
    hasher.update(ctx.ontology_snapshot_id.as_deref().unwrap_or("").as_bytes());
    let h = hasher.finalize();
    let hex: String = h.iter().take(8).map(|b| format!("{b:02x}")).collect();
    format!("ctx:{hex}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflow_contracts::workflow_intent::WorkflowIntent;

    #[test]
    fn proposal_id_is_deterministic() {
        let ctx = PlanningContext::new(WorkflowIntent {
            id: "s1".into(),
            schema_version: semver::Version::new(1, 0, 0),
            goal: "g".into(),
            modality: Some("bulk_rnaseq".into()),
            ..Default::default()
        });
        let a = proposal_id("insert_liftover", "g1", &ctx);
        let b = proposal_id("insert_liftover", "g1", &ctx);
        assert_eq!(a, b, "same (strategy, gap, ctx) must yield same id");
        let c = proposal_id("insert_liftover", "g2", &ctx);
        assert_ne!(a, c, "different gap must yield different id");
    }

    #[test]
    fn ctx_snapshot_hash_responds_to_modality() {
        let mut ctx = PlanningContext::new(WorkflowIntent {
            id: "s1".into(),
            schema_version: semver::Version::new(1, 0, 0),
            goal: "g".into(),
            modality: Some("bulk_rnaseq".into()),
            ..Default::default()
        });
        let h1 = ctx_snapshot_hash(&ctx);
        ctx.intent.modality = Some("variant_calling".into());
        let h2 = ctx_snapshot_hash(&ctx);
        assert_ne!(h1, h2);
    }
}
