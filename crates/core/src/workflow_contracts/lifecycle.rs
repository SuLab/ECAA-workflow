//! Node lifecycle and trust per design §7.
//!
//! Lifecycle gates are wired into emit/promotion paths. Default for
//! migrated atoms is `LifecycleState::Contracted` +
//! `TrustLevel::Unverified`; promotion to `Production`/`Trusted`
//! requires the full evidence set checked by `can_promote` in
//! `promotion_gate`. The intentionally cautious default prevents
//! silent bulk-promotion of untested atoms.
//!
//! The six non-monotonic lifecycle edges from design §7 live in the
//! crate-level [`crate::lifecycle_adversarial`] module. The
//! [`production_revocation_cascade`] helper here links a
//! `LifecycleState::Production → !Production` transition to the
//! adversarial detection.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::lifecycle_adversarial::LifecycleTransition;

/// Lifecycle state — which trust band a node sits in.
#[derive(
    Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, TS, Default, schemars::JsonSchema,
)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum LifecycleState {
    /// Concept proposed, contract not yet specified.
    Hypothesized,
    /// Contract specified (inputs/outputs/preconditions etc.) but
    /// no implementation yet.
    #[default]
    Contracted,
    /// Implementation present but not validated locally.
    Implemented,
    /// Local validation passed (contract tests, golden tests).
    LocallyValidated,
    /// Benchmark validation passed (benchmarks present in evidence set).
    BenchmarkValidated,
    /// Production-ready: required tests/benchmarks/container
    /// digests/human approval all in place.
    Production,
    /// Deprecated — composer warns or refuses to include.
    Deprecated,
}

impl LifecycleState {
    /// Stable order key for byte-stable scoring.
    pub fn order(self) -> u8 {
        match self {
            LifecycleState::Hypothesized => 0,
            LifecycleState::Contracted => 1,
            LifecycleState::Implemented => 2,
            LifecycleState::LocallyValidated => 3,
            LifecycleState::BenchmarkValidated => 4,
            LifecycleState::Production => 5,
            LifecycleState::Deprecated => 6,
        }
    }

    /// True if a node in this lifecycle state can execute as part
    /// of a production DAG (no `DraftDag`/`PartialDag`/`NovelNodeSpec`
    /// outcome).
    pub fn allows_production_execution(self) -> bool {
        matches!(
            self,
            LifecycleState::LocallyValidated
                | LifecycleState::BenchmarkValidated
                | LifecycleState::Production
        )
    }

    /// V4 alignment stable canonical snake_case key.
    /// Matches the keys used in `config/promotion-gate-policy.yaml`
    /// (`hypothesized`, `contracted`, `implemented`, `locally_validated`,
    /// `benchmark_validated`, `production`, `deprecated`) so the v4 P3
    /// promotion-gate loader can look up the requirements row for a
    /// target state without a re-mapping.
    pub fn canonical_name(self) -> &'static str {
        match self {
            LifecycleState::Hypothesized => "hypothesized",
            LifecycleState::Contracted => "contracted",
            LifecycleState::Implemented => "implemented",
            LifecycleState::LocallyValidated => "locally_validated",
            LifecycleState::BenchmarkValidated => "benchmark_validated",
            LifecycleState::Production => "production",
            LifecycleState::Deprecated => "deprecated",
        }
    }
}

/// Trust level — separate axis from lifecycle. A `Production` node
/// imported from a `LocalRegistry` may still be `Untrusted` until a
/// human signs off.
#[derive(
    Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, TS, Default, schemars::JsonSchema,
)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum TrustLevel {
    /// Newly imported / generated — no signoff.
    #[default]
    Unverified,
    /// Static analysis passed; no human review.
    StaticChecked,
    /// One human reviewed.
    Reviewed,
    /// Multiple humans reviewed (required for `RiskClass::Clinical`).
    DualReviewed,
}

/// Status of a node — separate from lifecycle. `Active` is the
/// happy path; `Disabled` excludes from planning; `Quarantined`
/// allows draft outcomes only.
#[derive(
    Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, TS, Default, schemars::JsonSchema,
)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum NodeStatus {
    #[default]
    /// Active variant.
    Active,
    /// Disabled variant.
    Disabled,
    /// Quarantined variant.
    Quarantined,
}

/// Authority that promoted a node along its lifecycle.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, TS, schemars::JsonSchema)]
#[ts(export)]
pub struct PromotionAuthority {
    /// Authority kind (`automated_ci`, `human`, `external_registry`).
    pub kind: String,
    /// Authority id (CI run id, human user id, registry name).
    pub id: String,
    /// Promotion timestamp (ISO 8601). May be redacted in exportable
    /// provenance when suppression policy applies.
    pub at: String,
}

/// Deprecation notice. Set when a node is sunset; composer warns
/// during draft compositions and refuses production.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, TS, schemars::JsonSchema)]
#[ts(export)]
pub struct Deprecation {
    /// Reason (free text).
    pub reason: String,
    /// Recommended replacement node id, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub replacement: Option<String>,
    /// ISO 8601 sunset date.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub sunset_at: Option<String>,
}

/// v3 P8 — emit a `ProductionNodeRevocation` transition when a node
/// drops out of `LifecycleState::Production`. The caller feeds the
/// prior and new lifecycle state plus the revocation reason and the
/// dag ids that referenced the node; the helper assembles the
/// transition payload. Returns `None` when the transition is **not**
/// a production revocation.
pub fn production_revocation_cascade(
    node_id: impl Into<String>,
    prior: LifecycleState,
    new: LifecycleState,
    reason: impl Into<String>,
    affected_dags: Vec<String>,
) -> Option<LifecycleTransition> {
    if matches!(prior, LifecycleState::Production) && !matches!(new, LifecycleState::Production) {
        Some(LifecycleTransition::ProductionNodeRevocation {
            node_id: node_id.into(),
            prior_state: match prior {
                LifecycleState::Production => "production".to_string(),
                _ => format!("{prior:?}").to_lowercase(),
            },
            reason: reason.into(),
            affected_dags,
        })
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lifecycle_order_is_stable() {
        assert!(LifecycleState::Contracted.order() < LifecycleState::Production.order());
        assert!(LifecycleState::Production.order() < LifecycleState::Deprecated.order());
    }

    #[test]
    fn production_execution_gate() {
        assert!(LifecycleState::Production.allows_production_execution());
        assert!(LifecycleState::LocallyValidated.allows_production_execution());
        assert!(!LifecycleState::Hypothesized.allows_production_execution());
        assert!(!LifecycleState::Deprecated.allows_production_execution());
    }

    #[test]
    fn production_revocation_cascade_fires_only_for_demotion() {
        let t = production_revocation_cascade(
            "align_reads",
            LifecycleState::Production,
            LifecycleState::Deprecated,
            "yanked by registry",
            vec!["dag_1".into()],
        );
        assert!(matches!(
            t,
            Some(LifecycleTransition::ProductionNodeRevocation { .. })
        ));
        let t = production_revocation_cascade(
            "align_reads",
            LifecycleState::Production,
            LifecycleState::Production,
            "unchanged",
            vec![],
        );
        assert!(t.is_none());
        let t = production_revocation_cascade(
            "align_reads",
            LifecycleState::Contracted,
            LifecycleState::Implemented,
            "implemented",
            vec![],
        );
        assert!(t.is_none());
    }

    #[test]
    fn round_trips() {
        let p = PromotionAuthority {
            kind: "human".into(),
            id: "alan".into(),
            at: "2026-05-08T12:00:00Z".into(),
        };
        let json = serde_json::to_string(&p).unwrap();
        let back: PromotionAuthority = serde_json::from_str(&json).unwrap();
        assert_eq!(p, back);

        let d = Deprecation {
            reason: "Replaced by foo_v2".into(),
            replacement: Some("foo_v2".into()),
            sunset_at: Some("2026-12-31".into()),
        };
        let json = serde_json::to_string(&d).unwrap();
        let back: Deprecation = serde_json::from_str(&json).unwrap();
        assert_eq!(d, back);
    }
}
