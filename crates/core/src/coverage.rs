//! `CoverageResult` — recall reconciliation. Computed ONLY from the
//! structured `result.json claims[]` verdicts (the deterministic path)
//! against the deterministic `ExpectedClaimManifest`. The regex/narrative
//! extractor output is NEVER an input here — that keeps the Inv-1
//! predicate free of heuristic input (determinism boundary).

use crate::claim_verifier::{ClaimStatus, ClaimVerdict};
use crate::expected_claim::{ExpectedClaimManifest, Requirement};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use ts_rs::TS;

/// Per-entity coverage outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS, schemars::JsonSchema)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum EntityCoverage {
    /// A structured claim addresses this entity and is `Verified`.
    Addressed,
    /// A structured claim addresses this entity but is `Mismatch`/`Unverifiable`.
    Unverifiable,
    /// No structured claim addresses this entity.
    Absent,
}

/// Reconciliation of the structured-claim verdicts against the manifest.
/// Carried into the signed sink so Inv 1 reads it as deterministic data.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS, schemars::JsonSchema)]
#[ts(export)]
pub struct CoverageResult {
    /// Total Required manifest entries.
    pub required_total: usize,
    /// Required entries matched to a Verified structured claim.
    pub required_addressed: usize,
    /// Required entries matched to a Mismatch/Unverifiable structured claim.
    pub required_unverifiable: usize,
    /// Required entries with no addressing structured claim.
    pub required_absent: usize,
    /// Per-entity breakdown for the Required entries (BTreeMap for determinism).
    pub per_entity: BTreeMap<String, EntityCoverage>,
}

/// True when a structured-claim verdict addresses the expected entity.
/// Matching is case-insensitive on the entity token OR the resolved
/// source table basename — the structured path sets `claim.source_table`
/// to the resolved table name, which is exactly the manifest's
/// `expected_output_table` for confirmatory stages.
fn verdict_addresses(verdict: &ClaimVerdict, expected: &crate::expected_claim::ExpectedClaim) -> bool {
    let want_entity = expected.entity.to_ascii_lowercase();
    let want_table = expected
        .expected_output_table
        .as_deref()
        .map(|t| t.to_ascii_lowercase());
    let got_entity = verdict.claim.entity.to_ascii_lowercase();
    let got_table = verdict
        .claim
        .source_table
        .as_deref()
        .map(|t| t.to_ascii_lowercase());
    if got_entity == want_entity {
        return true;
    }
    match (want_table, got_table) {
        (Some(w), Some(g)) => g.contains(&w) || w.contains(&g),
        _ => false,
    }
}

/// Reconcile the structured-claim verdicts against the manifest's
/// `Required` entries. ONLY structured-claim verdicts are passed in
/// (the caller runs `verify_structured_claims`, never the regex path).
pub fn reconcile_coverage(
    manifest: &ExpectedClaimManifest,
    structured_verdicts: &[ClaimVerdict],
) -> CoverageResult {
    let mut per_entity: BTreeMap<String, EntityCoverage> = BTreeMap::new();

    for expected in manifest
        .entries
        .iter()
        .filter(|e| e.requirement == Requirement::Required)
    {
        // Best outcome wins: a Verified verdict makes the entry Addressed
        // even if a different table's verdict was Unverifiable.
        let mut outcome = EntityCoverage::Absent;
        for v in structured_verdicts {
            if !verdict_addresses(v, expected) {
                continue;
            }
            let candidate = match &v.status {
                ClaimStatus::Verified => EntityCoverage::Addressed,
                ClaimStatus::Mismatch { .. } | ClaimStatus::Unverifiable { .. } => {
                    EntityCoverage::Unverifiable
                }
            };
            // Addressed (Verified) beats Unverifiable beats Absent.
            outcome = match (outcome, candidate) {
                (EntityCoverage::Addressed, _) => EntityCoverage::Addressed,
                (_, EntityCoverage::Addressed) => EntityCoverage::Addressed,
                (EntityCoverage::Unverifiable, _) | (_, EntityCoverage::Unverifiable) => {
                    EntityCoverage::Unverifiable
                }
                _ => EntityCoverage::Absent,
            };
            if outcome == EntityCoverage::Addressed {
                break;
            }
        }
        per_entity.insert(expected.entity.clone(), outcome);
    }

    let required_total = per_entity.len();
    let required_addressed = per_entity
        .values()
        .filter(|c| **c == EntityCoverage::Addressed)
        .count();
    let required_unverifiable = per_entity
        .values()
        .filter(|c| **c == EntityCoverage::Unverifiable)
        .count();
    let required_absent = per_entity
        .values()
        .filter(|c| **c == EntityCoverage::Absent)
        .count();

    CoverageResult {
        required_total,
        required_addressed,
        required_unverifiable,
        required_absent,
        per_entity,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::claim_contract::ClaimContract;
    use crate::claim_extractor::Claim;
    use crate::claim_verifier::ClaimStrength;
    use crate::expected_claim::ExpectedClaim;

    fn manifest(entities: &[(&str, Requirement)]) -> ExpectedClaimManifest {
        ExpectedClaimManifest {
            schema_version: "1".into(),
            entries: entities
                .iter()
                .map(|(e, r)| ExpectedClaim {
                    entity: (*e).into(),
                    contrast: None,
                    expected_output_table: Some((*e).into()),
                    requirement: *r,
                    edam_data: None,
                })
                .collect(),
        }
    }

    fn verdict(entity: &str, table: Option<&str>, status: ClaimStatus) -> ClaimVerdict {
        ClaimVerdict {
            claim: Claim {
                entity: entity.into(),
                direction: None,
                effect_size: None,
                pvalue: None,
                source_table: table.map(String::from),
                excerpt: String::new(),
                contract: ClaimContract::NumericTableLookup,
            },
            status,
            strength: ClaimStrength::Exploratory,
        }
    }

    #[test]
    fn verified_required_is_addressed() {
        let m = manifest(&[("differential_expression", Requirement::Required)]);
        let verdicts = vec![verdict(
            "differential_expression",
            Some("differential_expression"),
            ClaimStatus::Verified,
        )];
        let cov = reconcile_coverage(&m, &verdicts);
        assert_eq!(cov.required_total, 1);
        assert_eq!(cov.required_addressed, 1);
        assert_eq!(cov.required_unverifiable, 0);
        assert_eq!(cov.required_absent, 0);
        assert_eq!(
            cov.per_entity["differential_expression"],
            EntityCoverage::Addressed
        );
    }

    #[test]
    fn absent_required_is_a_recall_gap() {
        let m = manifest(&[("differential_expression", Requirement::Required)]);
        let cov = reconcile_coverage(&m, &[]);
        assert_eq!(cov.required_total, 1);
        assert_eq!(cov.required_absent, 1);
        assert_eq!(
            cov.per_entity["differential_expression"],
            EntityCoverage::Absent
        );
    }

    #[test]
    fn unverifiable_required_is_counted_separately() {
        let m = manifest(&[("variant_calling", Requirement::Required)]);
        let verdicts = vec![verdict(
            "variant_calling",
            None,
            ClaimStatus::Unverifiable {
                reason: "no table".into(),
            },
        )];
        let cov = reconcile_coverage(&m, &verdicts);
        assert_eq!(cov.required_unverifiable, 1);
        assert_eq!(
            cov.per_entity["variant_calling"],
            EntityCoverage::Unverifiable
        );
    }

    #[test]
    fn optional_entries_do_not_count_toward_required_total() {
        let m = manifest(&[
            ("differential_expression", Requirement::Required),
            ("pathway_enrichment", Requirement::Optional),
        ]);
        let cov = reconcile_coverage(&m, &[]);
        assert_eq!(cov.required_total, 1, "Optional excluded from Required total");
        assert!(!cov.per_entity.contains_key("pathway_enrichment"));
    }

    #[test]
    fn empty_input_with_required_manifest_is_all_absent() {
        // F5 floor: zero structured claims + non-empty Required manifest ⇒
        // every Required entry absent (the predicate in Inv 1 fails).
        let m = manifest(&[
            ("differential_expression", Requirement::Required),
            ("variant_calling", Requirement::Required),
        ]);
        let cov = reconcile_coverage(&m, &[]);
        assert_eq!(cov.required_total, 2);
        assert_eq!(cov.required_absent, 2);
        assert_eq!(cov.required_addressed, 0);
    }
}
