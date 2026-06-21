//! Failure classification and the failure set tracked across repair rounds.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

/// How a failure is triaged for repair. Each class carries a distinct
/// auto-repair budget and determinism policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RepairClass {
    /// Narrative text disagrees with a frozen result; mechanically correctable.
    NarrativeCorrection,
    /// A conformance/structural defect with a deterministic fix.
    ConformanceFix,
    /// Evidence matrix is missing rows that can be completed from results.
    EvidenceCompletion,
    /// A citation is malformed or unresolved.
    CitationFix,
    /// A coverage statement has a gap to close.
    CoverageGap,
    /// An analysis task must be re-run (agentic).
    AnalysisRerun,
    /// An equivalence check must be re-run (agentic).
    EquivalenceRerun,
    /// No automated path; escalate to human review.
    ReviewRequired,
}

impl RepairClass {
    /// Deterministic classes apply mechanically without invoking an agent.
    pub fn is_deterministic(&self) -> bool {
        matches!(self, RepairClass::NarrativeCorrection | RepairClass::ConformanceFix)
    }
}

/// Default number of auto-repair attempts permitted for `class` before the
/// failure is escalated. `ReviewRequired` is never auto-attempted (0).
pub fn default_budget(class: RepairClass) -> usize {
    match class {
        RepairClass::NarrativeCorrection
        | RepairClass::ConformanceFix
        | RepairClass::EquivalenceRerun => 1,
        RepairClass::CitationFix
        | RepairClass::EvidenceCompletion
        | RepairClass::CoverageGap
        | RepairClass::AnalysisRerun => 3,
        RepairClass::ReviewRequired => 0,
    }
}

/// Hard ceiling on total repair rounds regardless of per-class budgets.
pub const GLOBAL_ROUND_CAP: usize = 20;

/// Origin of a failure: either a claim/result mismatch or a named invariant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum FailureSource {
    /// A claim disagreed with the frozen evidence.
    ClaimMismatch,
    /// A named invariant failed; the string is the invariant name.
    InvariantFailure(String),
}

impl FailureSource {
    /// Stable kind discriminator used in the failure id. Independent of
    /// any free-form detail text so ids survive across rounds.
    fn kind(&self) -> String {
        match self {
            FailureSource::ClaimMismatch => "claim".to_string(),
            FailureSource::InvariantFailure(name) => format!("inv:{name}"),
        }
    }
}

/// Lifecycle status of a failure within the repair loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FailureStatus {
    /// Still eligible for repair attempts.
    Open,
    /// Successfully repaired.
    Resolved,
    /// Escalated to human review.
    InReview,
}

/// A single tracked failure. The `id` is stable across rounds: it is derived
/// from `task`, `subject`, and source kind only -- never from `detail`,
/// `retry_count`, or `status`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Failure {
    /// Stable content-addressed id.
    pub id: String,
    /// Where the failure came from.
    pub source: FailureSource,
    /// How it is triaged for repair.
    pub class: RepairClass,
    /// Task the failure belongs to.
    pub task: String,
    /// Subject of the failure (claim id, artifact, etc.).
    pub subject: String,
    /// Free-form detail; may change across rounds without changing `id`.
    pub detail: String,
    /// Number of repair attempts already spent on this failure.
    pub retry_count: usize,
    /// Lifecycle status.
    pub status: FailureStatus,
}

impl Failure {
    /// Build a failure with a stable id computed from `task`, `subject`, and
    /// the source kind. `retry_count` starts at 0 and `status` at `Open`.
    pub fn new(
        source: FailureSource,
        class: RepairClass,
        task: &str,
        subject: &str,
        detail: &str,
    ) -> Self {
        let mut key = Vec::new();
        key.extend_from_slice(task.as_bytes());
        key.push(0x00);
        key.extend_from_slice(subject.as_bytes());
        key.push(0x00);
        key.extend_from_slice(source.kind().as_bytes());
        let id = crate::hash_utils::sha256_short(&key, 16);
        Failure {
            id,
            source,
            class,
            task: task.to_string(),
            subject: subject.to_string(),
            detail: detail.to_string(),
            retry_count: 0,
            status: FailureStatus::Open,
        }
    }
}

/// The set of failures tracked across a repair run.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FailureSet(pub Vec<Failure>);

impl FailureSet {
    /// True when no failure remains unresolved.
    pub fn all_resolved(&self) -> bool {
        self.0
            .iter()
            .all(|f| f.status == FailureStatus::Resolved)
    }

    /// Failures still open and under their per-class budget. `budget` maps a
    /// class to its remaining-attempt ceiling (typically [`default_budget`]).
    pub fn open(&self, budget: impl Fn(RepairClass) -> usize) -> Vec<&Failure> {
        self.0
            .iter()
            .filter(|f| f.status == FailureStatus::Open && f.retry_count < budget(f.class))
            .collect()
    }

    /// All failure ids, deduplicated and sorted.
    pub fn ids(&self) -> BTreeSet<String> {
        self.0.iter().map(|f| f.id.clone()).collect()
    }

    /// Failures that are not resolved (open or in review).
    pub fn unresolved(&self) -> Vec<&Failure> {
        self.0
            .iter()
            .filter(|f| f.status != FailureStatus::Resolved)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn id_is_stable_across_detail_changes() {
        let a = Failure::new(
            FailureSource::ClaimMismatch,
            RepairClass::NarrativeCorrection,
            "task_a",
            "claim_7",
            "first detail",
        );
        let b = Failure::new(
            FailureSource::ClaimMismatch,
            RepairClass::NarrativeCorrection,
            "task_a",
            "claim_7",
            "completely different detail text",
        );
        assert_eq!(
            a.id, b.id,
            "id must depend only on task+subject+kind, not detail"
        );
    }

    #[test]
    fn id_differs_by_subject_and_by_kind() {
        let base = Failure::new(
            FailureSource::ClaimMismatch,
            RepairClass::NarrativeCorrection,
            "task_a",
            "claim_7",
            "d",
        );
        let other_subject = Failure::new(
            FailureSource::ClaimMismatch,
            RepairClass::NarrativeCorrection,
            "task_a",
            "claim_8",
            "d",
        );
        assert_ne!(
            base.id, other_subject.id,
            "different subject must yield different id"
        );
        let other_kind = Failure::new(
            FailureSource::InvariantFailure("schema".to_string()),
            RepairClass::ConformanceFix,
            "task_a",
            "claim_7",
            "d",
        );
        assert_ne!(
            base.id, other_kind.id,
            "different source kind must yield different id"
        );
    }

    #[test]
    fn invariant_kind_is_named() {
        let f = Failure::new(
            FailureSource::InvariantFailure("evidence_complete".to_string()),
            RepairClass::EvidenceCompletion,
            "t",
            "s",
            "d",
        );
        // Recompute the expected id from the documented recipe.
        let mut key = Vec::new();
        key.extend_from_slice(b"t");
        key.push(0x00);
        key.extend_from_slice(b"s");
        key.push(0x00);
        key.extend_from_slice(b"inv:evidence_complete");
        assert_eq!(
            f.id,
            crate::hash_utils::sha256_short(&key, 16),
            "invariant kind must serialize as inv:<name>"
        );
    }

    #[test]
    fn open_filters_by_status_and_budget() {
        let mut open_under = Failure::new(
            FailureSource::ClaimMismatch,
            RepairClass::CitationFix,
            "t",
            "s1",
            "d",
        );
        open_under.retry_count = 2; // budget 3 -> eligible

        let mut open_exhausted = Failure::new(
            FailureSource::ClaimMismatch,
            RepairClass::NarrativeCorrection,
            "t",
            "s2",
            "d",
        );
        open_exhausted.retry_count = 1; // budget 1 -> NOT eligible

        let mut resolved = Failure::new(
            FailureSource::ClaimMismatch,
            RepairClass::CitationFix,
            "t",
            "s3",
            "d",
        );
        resolved.status = FailureStatus::Resolved;

        let fs = FailureSet(vec![open_under.clone(), open_exhausted, resolved]);
        let open = fs.open(default_budget);
        assert_eq!(open.len(), 1, "only the under-budget open failure is eligible");
        assert_eq!(open[0].subject, "s1", "wrong failure selected by open()");
    }

    #[test]
    fn all_resolved_and_unresolved_partition() {
        let mut r = Failure::new(
            FailureSource::ClaimMismatch,
            RepairClass::CitationFix,
            "t",
            "s1",
            "d",
        );
        r.status = FailureStatus::Resolved;
        let o = Failure::new(
            FailureSource::ClaimMismatch,
            RepairClass::CitationFix,
            "t",
            "s2",
            "d",
        );
        let fs = FailureSet(vec![r.clone(), o]);
        assert!(!fs.all_resolved(), "one open failure means not all resolved");
        assert_eq!(fs.unresolved().len(), 1, "exactly one unresolved failure");

        let only_resolved = FailureSet(vec![r]);
        assert!(only_resolved.all_resolved(), "single resolved failure is all_resolved");
    }

    #[test]
    fn deterministic_classes() {
        assert!(RepairClass::NarrativeCorrection.is_deterministic());
        assert!(RepairClass::ConformanceFix.is_deterministic());
        assert!(!RepairClass::AnalysisRerun.is_deterministic());
        assert!(!RepairClass::ReviewRequired.is_deterministic());
    }

    #[test]
    fn budgets_match_spec() {
        assert_eq!(default_budget(RepairClass::NarrativeCorrection), 1);
        assert_eq!(default_budget(RepairClass::ConformanceFix), 1);
        assert_eq!(default_budget(RepairClass::EquivalenceRerun), 1);
        assert_eq!(default_budget(RepairClass::CitationFix), 3);
        assert_eq!(default_budget(RepairClass::EvidenceCompletion), 3);
        assert_eq!(default_budget(RepairClass::CoverageGap), 3);
        assert_eq!(default_budget(RepairClass::AnalysisRerun), 3);
        assert_eq!(default_budget(RepairClass::ReviewRequired), 0);
        assert_eq!(GLOBAL_ROUND_CAP, 20);
    }
}
