//! Type-aware repair classification for claim-verification failures.
//!
//! When a claim verifies as `Mismatch` (or soft `Suspicious`), the correct
//! response depends on WHAT failed — not a single "rerun the task" reflex. The
//! claim verifier checks the NARRATIVE against the task's RESULT TABLE, where
//! the table is the computed, deterministic artifact. So almost every
//! claim-verification failure is "the prose disagrees with the computed table",
//! whose cheap, deterministic fix is to CORRECT THE PROSE to match the table —
//! NOT to re-run the analysis (the table/analysis is the ground truth here).
//!
//! Re-execution is a DIFFERENT subsystem's job: it is the harness's bounded
//! response to ANALYSIS-validation failures (the `validate_*` tasks /
//! positive-negative controls / reference-range checks that test the
//! computation itself), gated by `--max-iterations`. A claim-verification
//! Mismatch is post-execution and never triggers task re-execution — which is
//! why [`RepairAction`] deliberately has NO `TaskReexecution` variant: a claim
//! failure is a narrative/evidence problem, not an analysis problem.
//!
//! This module turns the per-claim verdicts into an actionable, auditable
//! REPAIR PLAN (`runtime/claim-repair-plan.json`). It classifies each failure
//! and carries the verifier's own detail (which states the table's correct
//! value) so the fix is unambiguous. Applying the plan to scientific prose is
//! deliberately a separate, opt-in/human-gated step — a low false-positive
//! verifier is the precondition for trusting any auto-correction, and an
//! integrity tool must not silently rewrite scientific claims.

use crate::claim_contract::ClaimContract;
use crate::claim_verifier::{ClaimStatus, ClaimVerdict, ClaimVerificationReport};
use serde::{Deserialize, Serialize};

/// The correct repair response for one claim-verification failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RepairAction {
    /// The narrative misreports its own cited table (a count / effect size /
    /// p-value / direction / fold that disagrees with the computed table). The
    /// table is ground truth → correct the PROSE to match it. Deterministic,
    /// cheap, never re-runs the analysis.
    NarrativeCorrection,
    /// The claim cites an evidence FILE that does not exist / does not resolve.
    /// Fix the citation (or remove the claim) — the reference is broken.
    CitationFix,
    /// The claim may be true but is not backed by the recorded evidence (e.g. a
    /// literature citation with no supporting row in the evidence matrix).
    /// Complete the evidence (record the supporting source), do not touch the
    /// science.
    EvidenceCompletion,
    /// A soft `Suspicious` flag (quantitative claim about an entity absent from
    /// the cited table). Route to a human for review; never auto-acted.
    ReviewRequired,
}

impl RepairAction {
    /// Short label for the plan + logs.
    pub fn label(&self) -> &'static str {
        match self {
            RepairAction::NarrativeCorrection => "narrative_correction",
            RepairAction::CitationFix => "citation_fix",
            RepairAction::EvidenceCompletion => "evidence_completion",
            RepairAction::ReviewRequired => "review_required",
        }
    }
}

/// One actionable repair-plan entry derived from a failing verdict.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepairItem {
    /// The claim entity / subject as recorded on the verdict.
    pub entity: String,
    /// The claim text (narrative excerpt or structured claim).
    pub excerpt: String,
    /// The cited table/evidence, when the claim names one.
    pub source_table: Option<String>,
    /// Classified repair action.
    pub action: RepairAction,
    /// The verifier's own detail (states the table's correct value / the gap),
    /// so the correction is unambiguous and auditable.
    pub detail: String,
}

/// Classify the repair response for a single failing verdict. `Verified`
/// claims classify as `NarrativeCorrection` only when a caller passes them by
/// mistake — callers should pass only `Mismatch`/`Suspicious` verdicts (see
/// [`build_repair_plan`]).
pub fn classify(verdict: &ClaimVerdict) -> RepairAction {
    match &verdict.status {
        // Soft flag — always human review, never auto-acted.
        ClaimStatus::Suspicious { .. } => RepairAction::ReviewRequired,
        ClaimStatus::Mismatch { detail } => {
            // Literature concordance/citation failures are evidence problems,
            // not prose-vs-table arithmetic.
            if verdict.claim.contract == ClaimContract::LiteratureGrounded {
                return RepairAction::EvidenceCompletion;
            }
            // A cited evidence file that resolves nowhere is a broken citation.
            let d = detail.to_ascii_lowercase();
            if d.contains("does not exist") || d.contains("not found") || d.contains("phantom") {
                return RepairAction::CitationFix;
            }
            // Everything else the claim verifier emits is a narrative value /
            // direction / count disagreeing with the computed table → correct
            // the prose to the table.
            RepairAction::NarrativeCorrection
        }
        // Verified / Unverifiable are not repair targets.
        _ => RepairAction::ReviewRequired,
    }
}

/// Build the actionable repair plan for a finalized task report: one
/// [`RepairItem`] per `Mismatch` or `Suspicious` verdict, type-classified.
/// `Verified` and `Unverifiable` verdicts are not repair targets and are
/// skipped (an Unverifiable claim is neither confirmed nor refuted — there is
/// nothing to "fix").
pub fn build_repair_plan(report: &ClaimVerificationReport) -> Vec<RepairItem> {
    report
        .verdicts
        .iter()
        .filter(|v| matches!(v.status, ClaimStatus::Mismatch { .. } | ClaimStatus::Suspicious { .. }))
        .map(|v| {
            let detail = match &v.status {
                ClaimStatus::Mismatch { detail } => detail.clone(),
                ClaimStatus::Suspicious { reason } => reason.clone(),
                _ => String::new(),
            };
            RepairItem {
                entity: v.claim.entity.clone(),
                excerpt: v.claim.excerpt.clone(),
                source_table: v.claim.source_table.clone(),
                action: classify(v),
                detail,
            }
        })
        .collect()
}

/// Persist a task's repair plan into the package-level
/// `runtime/claim-repair-plan.json` (a map `task_id -> [RepairItem]`),
/// read-modify-write so re-finalize is idempotent (a task whose mismatches were
/// fixed drops out of the map). Purely INFORMATIONAL and non-destructive: it
/// records WHAT to fix and HOW (the verifier's detail carries the table's
/// correct value), never rewriting any narrative. Best-effort; the caller logs
/// failures and continues finalize.
pub fn persist_repair_plan(
    root: &std::path::Path,
    task_id: &str,
    report: &ClaimVerificationReport,
) -> std::io::Result<()> {
    use std::collections::BTreeMap;
    let path = root.join("runtime").join("claim-repair-plan.json");
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut map: BTreeMap<String, Vec<RepairItem>> = if path.exists() {
        serde_json::from_slice(&std::fs::read(&path)?).unwrap_or_default()
    } else {
        BTreeMap::new()
    };
    let items = build_repair_plan(report);
    if items.is_empty() {
        map.remove(task_id);
    } else {
        map.insert(task_id.to_string(), items);
    }
    std::fs::write(&path, serde_json::to_vec_pretty(&map)?)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::claim_extractor::Claim;
    use crate::claim_verifier::ClaimStrength;

    fn verdict(contract: ClaimContract, status: ClaimStatus, source: Option<&str>) -> ClaimVerdict {
        ClaimVerdict {
            claim: Claim {
                entity: "X".into(),
                direction: None,
                effect_size: None,
                pvalue: None,
                source_table: source.map(|s| s.to_string()),
                excerpt: "claim text".into(),
                contract,
                literature_evidence: None,
                matched_pvalue_keyword: None,
                linear_fold: None,
            },
            status,
            strength: ClaimStrength::Exploratory,
        }
    }

    #[test]
    fn classifies_each_failure_type() {
        // Count/value/direction vs table → narrative correction.
        assert_eq!(
            classify(&verdict(
                ClaimContract::ThresholdedDeOrEnrichment,
                ClaimStatus::Mismatch { detail: "count claim: narrative says 453, `t.tsv` has 334".into() },
                Some("t.tsv"),
            )),
            RepairAction::NarrativeCorrection
        );
        // Phantom citation → citation fix.
        assert_eq!(
            classify(&verdict(
                ClaimContract::ThresholdedDeOrEnrichment,
                ClaimStatus::Mismatch { detail: "claim cites evidence file `g.tsv` that does not exist anywhere".into() },
                None,
            )),
            RepairAction::CitationFix
        );
        // Literature → evidence completion.
        assert_eq!(
            classify(&verdict(
                ClaimContract::LiteratureGrounded,
                ClaimStatus::Mismatch { detail: "narrative cites PMID 1 but the matrix has no supporting row".into() },
                None,
            )),
            RepairAction::EvidenceCompletion
        );
        // Suspicious → review.
        assert_eq!(
            classify(&verdict(
                ClaimContract::NumericTableLookup,
                ClaimStatus::Suspicious { reason: "absent-entity quantitative claim".into() },
                Some("t.tsv"),
            )),
            RepairAction::ReviewRequired
        );
    }

    #[test]
    fn plan_skips_verified_and_unverifiable() {
        let mut report = ClaimVerificationReport::empty();
        report.push(verdict(ClaimContract::NumericTableLookup, ClaimStatus::Verified, Some("t.tsv")));
        report.push(verdict(
            ClaimContract::NumericTableLookup,
            ClaimStatus::Unverifiable { reason: "no table".into() },
            None,
        ));
        report.push(verdict(
            ClaimContract::ThresholdedDeOrEnrichment,
            ClaimStatus::Mismatch { detail: "count claim: narrative says 5, `t.tsv` has 3".into() },
            Some("t.tsv"),
        ));
        let plan = build_repair_plan(&report);
        assert_eq!(plan.len(), 1, "only the Mismatch is a repair target");
        assert_eq!(plan[0].action, RepairAction::NarrativeCorrection);
    }
}
