//! Policy engine.
//!
//! `PolicyContext` is the typed, composer-readable form of
//! `config/downstream-policy/*.json`. It feeds:
//!
//! - `CompatibilityEngine` — policy gate adds an
//!   `Incompatible(PrivacyClassWidening | ClinicalGateFailed)`
//!   when the proposed edge violates the active context.
//! - planner scoring — `ScoringTuple.hard_policy_violation`
//!   flips to `Reject` when a proof carries a policy refusal.
//! - `CompatibilityProof.policy_decisions` — every accepted
//!   policy check is recorded inline so RO-Crate provenance
//!   emits the audit trail.
//!
//! The type shape is wired into the engine and planner. Each policy
//! is a typed predicate so composer code branches on policy *kind*
//! rather than on free-text strings.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use ts_rs::TS;

use crate::population_coverage::PopulationWaiver;
use crate::workflow_contracts::data_product::PrivacyClass;

/// Top-level policy context. Sessions opt in to one or more
/// policy bundles; the composer evaluates every active bundle
/// for each edge.
#[derive(
    Debug, Clone, Serialize, Deserialize, PartialEq, Eq, TS, Default, schemars::JsonSchema,
)]
#[ts(export)]
pub struct PolicyContext {
    /// Active policy bundles by id (`clinical_v1`, `phi_strict`,
    /// `cfr_part11_v2`).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub bundles: BTreeMap<String, PolicyBundle>,
    /// Default privacy class applied to data products with no
    /// explicit class.
    #[serde(default)]
    pub default_privacy_class: PrivacyClass,
}

/// One policy bundle. Bundles are loaded from
/// `config/downstream-policy/*.json` at composer startup.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, TS, schemars::JsonSchema)]
#[ts(export)]
pub struct PolicyBundle {
    /// Stable id (filename without the `.json`).
    pub id: String,
    /// Human label (rendered in UI policy cards).
    pub label: String,
    /// Free-text description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub description: Option<String>,
    /// Active policy checks.
    pub checks: Vec<PolicyCheck>,
    /// Regulatory citation (e.g. "21 CFR Part 11 §11.10(a)").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub regulatory_citation: Option<String>,
    /// v3 P9 §11.X — population waivers signed against this bundle.
    /// Empty for fresh bundles; populated via the
    /// `POST /api/chat/session/:id/population-waiver` endpoint when the
    /// SME's clinical lead signs off on an out-of-coverage sample
    /// cohort. The composer's `classify_outcome_with_policy` consults
    /// this list before emitting a `PopulationOutOfCoverage` refusal;
    /// a matching waiver suppresses the refusal and the decision is
    /// emitted into `runtime/decisions.jsonl` as durable provenance.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub population_waivers: Vec<PopulationWaiver>,
}

/// A single typed policy check.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, TS, schemars::JsonSchema)]
#[ts(export)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PolicyCheck {
    /// Refuse executable status if any node is not in
    /// `LifecycleState::{LocallyValidated, BenchmarkValidated,
    /// Production}`. The clinical gate.
    ValidatedNodesOnly,
    /// Require every container to carry a pinned digest.
    RequirePinnedContainers,
    /// Refuse generated-code implementation entirely.
    NoGeneratedCode,
    /// Refuse adapters classified as `PolicyRestricted`.
    NoPolicyRestrictedAdapters,
    /// Refuse adapters classified as `ScientificallyRisky`.
    NoScientificallyRiskyAdapters,
    /// Refuse edges that widen privacy class (PHI → public).
    NoPrivacyWidening,
    /// Refuse network access in any executor task. Mirrors
    /// `ContainerSpec.network = None`.
    NoNetwork,
    /// Refuse paths whose nodes use unpinned reference data
    /// (genome assembly without patch number, annotation without
    /// release).
    PinnedReferenceDataOnly,
    /// Require human signoff before executable status. Validation
    /// reports must include a `HumanReviewed` trust stamp.
    HumanSignoffRequired,
    /// Require an audit trail (decisions.jsonl entries for every
    /// non-default decision).
    AuditTrailRequired,
    /// Free-form site-local check. Composer treats as
    /// pass-through warning (UI shows the statement).
    SiteLocal { check_id: String, statement: String },
}

impl PolicyContext {
    /// Empty.
    pub fn empty() -> Self {
        Self::default()
    }

    /// Convenience: build the canonical clinical-trial policy
    /// bundle. Mirrors the rules embedded in
    /// `config/downstream-policy/clinical-trial-analysis.json`
    /// without re-parsing it (the JSON file remains the source
    /// of truth at runtime).
    pub fn clinical_trial_bundle() -> PolicyBundle {
        PolicyBundle {
            id: "clinical_trial".into(),
            label: "Clinical-trial analysis".into(),
            description: Some(
                "Validated pipelines only, pinned containers, audit trail, no generated \
                 code, no policy-restricted adapters, no network."
                    .into(),
            ),
            checks: vec![
                PolicyCheck::ValidatedNodesOnly,
                PolicyCheck::RequirePinnedContainers,
                PolicyCheck::NoGeneratedCode,
                PolicyCheck::NoPolicyRestrictedAdapters,
                PolicyCheck::NoNetwork,
                PolicyCheck::PinnedReferenceDataOnly,
                PolicyCheck::HumanSignoffRequired,
                PolicyCheck::AuditTrailRequired,
                PolicyCheck::NoPrivacyWidening,
            ],
            regulatory_citation: Some("21 CFR Part 11; ICH E9(R1)".into()),
            population_waivers: Vec::new(),
        }
    }

    /// Convenience: build a PHI/HIPAA bundle.
    pub fn phi_strict_bundle() -> PolicyBundle {
        PolicyBundle {
            id: "phi_strict".into(),
            label: "PHI strict".into(),
            description: Some(
                "PHI data may not pass into network-enabled or generated-code nodes; \
                 privacy widening is refused."
                    .into(),
            ),
            checks: vec![
                PolicyCheck::NoPrivacyWidening,
                PolicyCheck::NoGeneratedCode,
                PolicyCheck::NoNetwork,
                PolicyCheck::AuditTrailRequired,
            ],
            regulatory_citation: Some("HIPAA 45 CFR §164.502, §164.514".into()),
            population_waivers: Vec::new(),
        }
    }

    /// Activate a bundle.
    pub fn with_bundle(mut self, bundle: PolicyBundle) -> Self {
        self.bundles.insert(bundle.id.clone(), bundle);
        self
    }

    /// Iterate every active check across all bundles in stable
    /// order.
    pub fn iter_checks(&self) -> impl Iterator<Item = (&str, &PolicyCheck)> {
        self.bundles
            .values()
            .flat_map(|b| b.checks.iter().map(move |c| (b.id.as_str(), c)))
    }

    /// True when at least one active bundle requires the named
    /// check kind. Composer uses this to short-circuit "is this
    /// session clinical?" decisions without iterating all bundles.
    pub fn requires_check(&self, kind: PolicyCheckKind) -> bool {
        self.iter_checks().any(|(_, c)| c.kind() == kind)
    }
}

/// Stable-discriminator enum so callers can ask "does any
/// bundle require X?" without pattern-matching the full
/// `PolicyCheck`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, TS, schemars::JsonSchema)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum PolicyCheckKind {
    /// ValidatedNodesOnly variant.
    ValidatedNodesOnly,
    /// RequirePinnedContainers variant.
    RequirePinnedContainers,
    /// NoGeneratedCode variant.
    NoGeneratedCode,
    /// NoPolicyRestrictedAdapters variant.
    NoPolicyRestrictedAdapters,
    /// NoScientificallyRiskyAdapters variant.
    NoScientificallyRiskyAdapters,
    /// NoPrivacyWidening variant.
    NoPrivacyWidening,
    /// NoNetwork variant.
    NoNetwork,
    /// PinnedReferenceDataOnly variant.
    PinnedReferenceDataOnly,
    /// HumanSignoffRequired variant.
    HumanSignoffRequired,
    /// AuditTrailRequired variant.
    AuditTrailRequired,
    /// SiteLocal variant.
    SiteLocal,
}

impl PolicyCheck {
    /// Kind.
    pub fn kind(&self) -> PolicyCheckKind {
        match self {
            PolicyCheck::ValidatedNodesOnly => PolicyCheckKind::ValidatedNodesOnly,
            PolicyCheck::RequirePinnedContainers => PolicyCheckKind::RequirePinnedContainers,
            PolicyCheck::NoGeneratedCode => PolicyCheckKind::NoGeneratedCode,
            PolicyCheck::NoPolicyRestrictedAdapters => PolicyCheckKind::NoPolicyRestrictedAdapters,
            PolicyCheck::NoScientificallyRiskyAdapters => {
                PolicyCheckKind::NoScientificallyRiskyAdapters
            }
            PolicyCheck::NoPrivacyWidening => PolicyCheckKind::NoPrivacyWidening,
            PolicyCheck::NoNetwork => PolicyCheckKind::NoNetwork,
            PolicyCheck::PinnedReferenceDataOnly => PolicyCheckKind::PinnedReferenceDataOnly,
            PolicyCheck::HumanSignoffRequired => PolicyCheckKind::HumanSignoffRequired,
            PolicyCheck::AuditTrailRequired => PolicyCheckKind::AuditTrailRequired,
            PolicyCheck::SiteLocal { .. } => PolicyCheckKind::SiteLocal,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_context_has_no_checks() {
        let ctx = PolicyContext::empty();
        assert_eq!(ctx.iter_checks().count(), 0);
        assert!(!ctx.requires_check(PolicyCheckKind::ValidatedNodesOnly));
    }

    #[test]
    fn clinical_bundle_requires_validated_nodes() {
        let ctx = PolicyContext::empty().with_bundle(PolicyContext::clinical_trial_bundle());
        assert!(ctx.requires_check(PolicyCheckKind::ValidatedNodesOnly));
        assert!(ctx.requires_check(PolicyCheckKind::RequirePinnedContainers));
        assert!(ctx.requires_check(PolicyCheckKind::NoGeneratedCode));
        assert!(ctx.requires_check(PolicyCheckKind::HumanSignoffRequired));
    }

    #[test]
    fn phi_strict_bundle_requires_no_privacy_widening() {
        let ctx = PolicyContext::empty().with_bundle(PolicyContext::phi_strict_bundle());
        assert!(ctx.requires_check(PolicyCheckKind::NoPrivacyWidening));
        assert!(ctx.requires_check(PolicyCheckKind::NoNetwork));
        assert!(!ctx.requires_check(PolicyCheckKind::ValidatedNodesOnly));
    }

    #[test]
    fn bundles_compose() {
        let ctx = PolicyContext::empty()
            .with_bundle(PolicyContext::clinical_trial_bundle())
            .with_bundle(PolicyContext::phi_strict_bundle());
        assert!(ctx.requires_check(PolicyCheckKind::ValidatedNodesOnly));
        assert!(ctx.requires_check(PolicyCheckKind::NoPrivacyWidening));
    }

    #[test]
    fn round_trip_round_trip_serde() {
        let ctx = PolicyContext::empty().with_bundle(PolicyContext::clinical_trial_bundle());
        let json = serde_json::to_string(&ctx).unwrap();
        let back: PolicyContext = serde_json::from_str(&json).unwrap();
        assert_eq!(ctx, back);
    }

    #[test]
    fn site_local_check_round_trips() {
        let mut bundle = PolicyContext::clinical_trial_bundle();
        bundle.checks.push(PolicyCheck::SiteLocal {
            check_id: "site_data_residency".into(),
            statement: "Data must remain within US-East".into(),
        });
        let json = serde_json::to_string(&bundle).unwrap();
        let back: PolicyBundle = serde_json::from_str(&json).unwrap();
        assert_eq!(bundle, back);
    }
}
