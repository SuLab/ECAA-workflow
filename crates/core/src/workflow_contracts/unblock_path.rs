//! `UnblockPath` — actionable recovery affordance attached to every
//! non-hard `RefusalReport`.
//!
//! An `UnblockPath` is a typed recovery affordance the SME can
//! dispatch via the `/api/chat/session/:id/refusal/:refusal_id/dispatch`
//! endpoint. Each variant carries both the data the dispatch endpoint
//! needs (assumption id, rule id, gap id, etc.) and a `ProjectedOutcome`
//! the UI can render so the SME knows what outcome to expect if they
//! choose this path.
//!
//! Every variant except `AttemptRepair` is fully wired for dispatch.
//! `AttemptRepair` is defined in the registry but its dispatch endpoint
//! returns `501 NOT_IMPLEMENTED` — the variant is emitted when a repair
//! candidate is in scope; actual dispatch is not yet wired.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// One actionable recovery path for an SME-facing refusal.
///
/// Every variant carries a `target_outcome: ProjectedOutcome` so the
/// UI can preview what the dispatch will produce. The dispatch endpoint
/// resolves the path's identifiers (assumption id, rule id, gap id,
/// field, reviewer class) against the session's state and the loaded
/// configuration tables (`assumption-policy.yaml`,
/// `credential-classes.yaml`, repair-strategy registry).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, TS, schemars::JsonSchema)]
#[ts(export)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum UnblockPath {
    /// Resolve a blocking assumption with a typed value (e.g. supply
    /// the genome build for a `GenomeBuildMismatch` assumption).
    ResolveAssumption {
        /// Assumption id.
        assumption_id: String,
        /// Optional canned suggestion string (UI prefills the input
        /// field with this).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        suggested_resolution: Option<String>,
        /// Target outcome.
        target_outcome: ProjectedOutcome,
    },
    /// Waive a policy rule with a credentialed sign-off. `rule_id`
    /// keys into `config/assumption-policy.yaml`;
    /// `required_credentials` references `config/credential-classes.yaml`.
    Waiver {
        /// Rule id.
        rule_id: String,
        /// Required credentials.
        required_credentials: Vec<String>,
        /// Target outcome.
        target_outcome: ProjectedOutcome,
    },
    /// Apply a registered repair strategy to close a typed gap.
    /// Emitted when a repair candidate is in scope; the dispatch
    /// endpoint returns `501 NOT_IMPLEMENTED` — actual dispatch
    /// is not yet wired.
    AttemptRepair {
        /// Strategy id.
        strategy_id: String,
        /// Gap id.
        gap_id: String,
        /// Target outcome.
        target_outcome: ProjectedOutcome,
    },
    /// Supply a piece of intake metadata the planner needs to
    /// converge (e.g. SME hadn't named a modality; planner can't
    /// pick an archetype).
    SupplyMissingMetadata {
        /// Field.
        field: String,
        /// Optional canned suggestion the UI prefills.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        suggested_value: Option<String>,
        /// Target outcome.
        target_outcome: ProjectedOutcome,
    },
    /// Escalate to a credentialed reviewer. `reviewer_class` keys
    /// into `config/credential-classes.yaml`.
    EscalateToReviewer {
        /// Reviewer class.
        reviewer_class: String,
        /// Required artifacts.
        required_artifacts: Vec<String>,
        /// Target outcome.
        target_outcome: ProjectedOutcome,
    },
}

/// Outcome the dispatch is projected to produce — the UI uses this to
/// preview the recovery. Maps 1:1 onto `ComposeOutcome`'s success
/// variants so the SME can read "Waive → ValidatedExecutableDag" as
/// "approving this waiver will produce an executable DAG."
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, TS, schemars::JsonSchema)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum ProjectedOutcome {
    /// Recovery will produce a `ValidatedExecutableDag` (fully
    /// resolved, ready to dispatch).
    ValidatedExecutableDag,
    /// Recovery will produce a `DraftDag` (assumptions still pending
    /// but the DAG is connected).
    DraftDag,
    /// Recovery will produce a `PartialDag` (some producer still
    /// absent; further work needed).
    PartialDag,
    /// Recovery will produce a `NovelNodeSpec` (the planner will
    /// propose a hypothesized node).
    NovelNodeSpec,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_resolve_assumption() {
        let p = UnblockPath::ResolveAssumption {
            assumption_id: "a:genome_build".into(),
            suggested_resolution: Some("GRCh38".into()),
            target_outcome: ProjectedOutcome::ValidatedExecutableDag,
        };
        let json = serde_json::to_string(&p).unwrap();
        let back: UnblockPath = serde_json::from_str(&json).unwrap();
        assert_eq!(p, back);
    }

    #[test]
    fn round_trips_waiver() {
        let p = UnblockPath::Waiver {
            rule_id: "assumption_policy:GenomeBuildMismatch:Phi".into(),
            required_credentials: vec!["clinical_lead".into(), "regulatory_officer".into()],
            target_outcome: ProjectedOutcome::DraftDag,
        };
        let json = serde_json::to_string(&p).unwrap();
        let back: UnblockPath = serde_json::from_str(&json).unwrap();
        assert_eq!(p, back);
    }

    #[test]
    fn round_trips_attempt_repair() {
        let p = UnblockPath::AttemptRepair {
            strategy_id: "insert_genome_build_inference".into(),
            gap_id: "g1".into(),
            target_outcome: ProjectedOutcome::PartialDag,
        };
        let json = serde_json::to_string(&p).unwrap();
        let back: UnblockPath = serde_json::from_str(&json).unwrap();
        assert_eq!(p, back);
    }

    #[test]
    fn round_trips_supply_missing_metadata() {
        let p = UnblockPath::SupplyMissingMetadata {
            field: "modality".into(),
            suggested_value: Some("single_cell_rnaseq".into()),
            target_outcome: ProjectedOutcome::DraftDag,
        };
        let json = serde_json::to_string(&p).unwrap();
        let back: UnblockPath = serde_json::from_str(&json).unwrap();
        assert_eq!(p, back);
    }

    #[test]
    fn round_trips_escalate_to_reviewer() {
        let p = UnblockPath::EscalateToReviewer {
            reviewer_class: "clinical_lead".into(),
            required_artifacts: vec!["IRB_approval.pdf".into()],
            target_outcome: ProjectedOutcome::NovelNodeSpec,
        };
        let json = serde_json::to_string(&p).unwrap();
        let back: UnblockPath = serde_json::from_str(&json).unwrap();
        assert_eq!(p, back);
    }

    #[test]
    fn projected_outcome_round_trip() {
        for o in [
            ProjectedOutcome::ValidatedExecutableDag,
            ProjectedOutcome::DraftDag,
            ProjectedOutcome::PartialDag,
            ProjectedOutcome::NovelNodeSpec,
        ] {
            let s = serde_json::to_string(&o).unwrap();
            let back: ProjectedOutcome = serde_json::from_str(&s).unwrap();
            assert_eq!(o, back);
        }
    }
}
