//! M5 — `runtime/coverage-statement.json`: a durable per-package
//! declaration of which goal branches were satisfied by CATALOG atoms vs
//! covered by `propose_hypothesized_node` (session proposals). The
//! artifact a reviewer reads to trust the package: it makes the
//! "communicate uncertainty when coverage lacks" property auditable,
//! rather than a transient UI banner.
//!
//! Computed deterministically at emit time from the emitted DAG's node
//! ids and the session's proposal node ids. No timestamps, BTreeMap +
//! sorted Vec ordering — byte-reproducible, so it stays IN the byte-diff
//! baseline.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use ts_rs::TS;

/// How a single goal branch (one DAG node) was covered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS, schemars::JsonSchema)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum BranchCoverage {
    /// Satisfied by a catalog atom / archetype-derived node.
    Catalog,
    /// Covered by a session proposal (`propose_hypothesized_node` or a
    /// composer-synthesized unsatisfiable-modality proposal). Unverified
    /// until promotion evidence accumulates.
    Proposal,
}

/// Durable goal-branch coverage statement emitted as
/// `runtime/coverage-statement.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS, schemars::JsonSchema)]
#[ts(export)]
pub struct CoverageStatement {
    /// Total goal branches considered (one per emitted DAG node).
    pub total_branches: usize,
    /// Branches satisfied by catalog atoms.
    pub catalog_covered: usize,
    /// Branches covered by a session proposal.
    pub proposal_covered: usize,
    /// True when every branch is catalog-covered (no proposal coverage).
    /// The headline trust signal: a reviewer reads this first.
    pub fully_catalog_covered: bool,
    /// Sorted list of the proposal-covered branch ids, so a reviewer sees
    /// exactly which strands rest on unverified proposals.
    pub proposal_branches: Vec<String>,
    /// Per-branch breakdown (BTreeMap for determinism).
    pub per_branch: BTreeMap<String, BranchCoverage>,
}

/// Reconcile emitted DAG node ids against the set of session-proposal
/// node ids. A node id present in `proposal_node_ids` is `Proposal`-
/// covered; every other emitted node is `Catalog`-covered.
pub fn build_coverage_statement(
    dag_node_ids: &[String],
    proposal_node_ids: &[String],
) -> CoverageStatement {
    let proposals: std::collections::BTreeSet<&str> =
        proposal_node_ids.iter().map(String::as_str).collect();

    let mut per_branch: BTreeMap<String, BranchCoverage> = BTreeMap::new();
    for id in dag_node_ids {
        let coverage = if proposals.contains(id.as_str()) {
            BranchCoverage::Proposal
        } else {
            BranchCoverage::Catalog
        };
        per_branch.insert(id.clone(), coverage);
    }

    let total_branches = per_branch.len();
    let proposal_covered = per_branch
        .values()
        .filter(|c| **c == BranchCoverage::Proposal)
        .count();
    let catalog_covered = total_branches - proposal_covered;
    let mut proposal_branches: Vec<String> = per_branch
        .iter()
        .filter(|(_, c)| **c == BranchCoverage::Proposal)
        .map(|(id, _)| id.clone())
        .collect();
    proposal_branches.sort();

    CoverageStatement {
        total_branches,
        catalog_covered,
        proposal_covered,
        fully_catalog_covered: proposal_covered == 0,
        proposal_branches,
        per_branch,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_only_nodes_are_all_catalog_covered() {
        let stmt = build_coverage_statement(
            &[
                "bulk_rnaseq_differential_expression".into(),
                "final_reporting".into(),
            ],
            &[], // no proposal node ids
        );
        assert_eq!(stmt.total_branches, 2);
        assert_eq!(stmt.catalog_covered, 2);
        assert_eq!(stmt.proposal_covered, 0);
        assert!(stmt.proposal_branches.is_empty());
        assert!(stmt.fully_catalog_covered);
    }

    #[test]
    fn proposal_nodes_are_attributed_to_proposal_coverage() {
        let stmt = build_coverage_statement(
            &[
                "bulk_rnaseq_differential_expression".into(),
                "cytof_pipeline".into(),
            ],
            &["cytof_pipeline".into()],
        );
        assert_eq!(stmt.total_branches, 2);
        assert_eq!(stmt.catalog_covered, 1);
        assert_eq!(stmt.proposal_covered, 1);
        assert_eq!(stmt.proposal_branches, vec!["cytof_pipeline".to_string()]);
        assert!(!stmt.fully_catalog_covered);
    }

    /// Determinism: the per-branch map is a BTreeMap and the proposal
    /// list is sorted, so re-running on the same inputs is byte-stable.
    #[test]
    fn output_is_deterministically_ordered() {
        let a = build_coverage_statement(
            &["z_node".into(), "a_node".into(), "m_node".into()],
            &["m_node".into(), "a_node".into()],
        );
        let b = build_coverage_statement(
            &["m_node".into(), "z_node".into(), "a_node".into()],
            &["a_node".into(), "m_node".into()],
        );
        assert_eq!(
            serde_json::to_string(&a).unwrap(),
            serde_json::to_string(&b).unwrap(),
            "same inputs (any order) must serialize identically"
        );
        assert_eq!(
            a.proposal_branches,
            vec!["a_node".to_string(), "m_node".to_string()]
        );
    }
}
