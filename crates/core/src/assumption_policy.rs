//! Assumption-policy table per v3 design §15.2.
//!
//! `(defect_class × privacy_class) → ResolutionPolicy`.
//!
//! Loaded into `PlanningContext` at compose time; consumed by
//! `composer_v4::planner::classify_outcome_with_policy` to drive
//! blocking decisions. Also consumed by v4 (`UnblockPath::Waiver`)
//! for authority routing.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use ts_rs::TS;

#[derive(
    Debug,
    Clone,
    Copy,
    Serialize,
    Deserialize,
    PartialEq,
    Eq,
    Ord,
    PartialOrd,
    Hash,
    TS,
    schemars::JsonSchema,
)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
/// DefectClass discriminant.
pub enum DefectClass {
    /// PrivacyViolation variant.
    PrivacyViolation,
    /// GenomeBuildMismatch variant.
    GenomeBuildMismatch,
    /// StrandednessUnknown variant.
    StrandednessUnknown,
    /// AnnotationVersionUnknown variant.
    AnnotationVersionUnknown,
    /// CoordinateConventionUnknown variant.
    CoordinateConventionUnknown,
    /// LibraryLayoutUnknown variant.
    LibraryLayoutUnknown,
    /// SampleMetadataMissing variant.
    SampleMetadataMissing,
    /// ReferenceDataUnspecified variant.
    ReferenceDataUnspecified,
    /// NovelNodeUnverified variant.
    NovelNodeUnverified,
    /// ScientificallyRiskyAdapter variant.
    ScientificallyRiskyAdapter,
    /// PolicyRestrictedAdapter variant.
    PolicyRestrictedAdapter,
    /// OntologyMappingUnresolved variant.
    OntologyMappingUnresolved,
    /// LossyAdapterDefault variant.
    LossyAdapterDefault,
}

#[derive(
    Debug,
    Clone,
    Copy,
    Serialize,
    Deserialize,
    PartialEq,
    Eq,
    Ord,
    PartialOrd,
    Hash,
    TS,
    schemars::JsonSchema,
)]
#[ts(export)]
#[ts(rename = "AssumptionPolicyPrivacyClass")]
#[serde(rename_all = "snake_case")]
/// PolicyPrivacyClass discriminant.
pub enum PolicyPrivacyClass {
    /// Phi variant.
    Phi,
    /// ControlledAccess variant.
    ControlledAccess,
    /// Research variant.
    Research,
    /// Public variant.
    Public,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, TS, schemars::JsonSchema)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
/// ResolutionPolicy discriminant.
pub enum ResolutionPolicy {
    /// Blocking variant.
    Blocking,
    /// NonBlocking variant.
    NonBlocking,
    /// WaivableWithCredentials variant.
    WaivableWithCredentials,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, TS, schemars::JsonSchema)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
/// RequiredResolutionState discriminant.
pub enum RequiredResolutionState {
    /// DraftOnly variant.
    DraftOnly,
    /// ValidatedOnly variant.
    ValidatedOnly,
    /// Any variant.
    Any,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, TS, schemars::JsonSchema)]
#[ts(export)]
/// PolicyEntry data.
pub struct PolicyEntry {
    /// Defect class.
    pub defect_class: DefectClass,
    /// Privacy class.
    pub privacy_class: PolicyPrivacyClass,
    /// Resolution.
    pub resolution: ResolutionPolicy,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    /// Required credentials.
    pub required_credentials: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    /// Required resolution state.
    pub required_resolution_state: Option<RequiredResolutionState>,
}

/// v3 §10.2 follow-up — one row in the `policy_rules:` registry. Every
/// `PolicyRuleId` referenced by a `ChainOfCustody`, `DecisionRecord`,
/// `PopulationWaiver`, or `LifecycleTransition::ForbiddenWaiverAttempt`
/// must construct against a row here.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, TS, schemars::JsonSchema)]
#[ts(export)]
pub struct PolicyRule {
    /// Stable foreign-key id. Snake-case ASCII with optional `:` for
    /// namespace separation (e.g. `phi_strict_v1`,
    /// `genome_build_mismatch:research`).
    pub id: String,
    /// Human-facing description used by the operator UI and the
    /// adjudication queue card.
    pub description: String,
    /// Privacy class this rule scopes to. Reuses the existing
    /// `PolicyPrivacyClass` enum so the registry and the
    /// `(defect_class × privacy_class)` entry table share a vocabulary.
    pub privacy_class: PolicyPrivacyClass,
    /// Defect class this rule scopes to. Reuses the existing
    /// `DefectClass` enum.
    pub defect_class: DefectClass,
}

#[derive(
    Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq, TS, schemars::JsonSchema,
)]
#[ts(export)]
/// AssumptionPolicyTable data.
pub struct AssumptionPolicyTable {
    /// Version.
    pub version: String,
    #[ts(skip)]
    entries: BTreeMap<(DefectClass, PolicyPrivacyClass), PolicyEntry>,
    /// v3 §10.2 follow-up — `policy_rules:` registry. Keyed by
    /// [`PolicyRule::id`] so look-ups are O(log n). Empty when the
    /// loaded YAML predates the current schema bump.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    rules: BTreeMap<String, PolicyRule>,
}

#[derive(thiserror::Error, Debug)]
#[non_exhaustive]
/// AssumptionPolicyError discriminant.
pub enum AssumptionPolicyError {
    #[error("file not found: {0}")]
    /// NotFound variant.
    NotFound(String),
    #[error("yaml parse: {0}")]
    /// YamlParse variant.
    YamlParse(#[from] serde_yml::Error),
    #[error("duplicate entry for ({0:?}, {1:?})")]
    /// Duplicate variant.
    Duplicate(DefectClass, PolicyPrivacyClass),
    /// v3 §10.2 follow-up — two `policy_rules:` rows share the same `id`.
    #[error("duplicate policy_rules entry for {0:?}")]
    DuplicateRule(String),
    #[error("io: {0}")]
    /// Io variant.
    Io(#[from] std::io::Error),
}

#[derive(Debug, Clone, Deserialize, schemars::JsonSchema)]
struct RawTable {
    version: String,
    entries: Vec<PolicyEntry>,
    /// `policy_rules:` registry. Optional in the YAML for back-compat
    /// with sessions emitted before the current schema; the
    /// JSON-schema sidecar (`config/_assumption-policy.schema.json`)
    /// requires the block on all newly-authored tables.
    #[serde(default)]
    policy_rules: Vec<PolicyRule>,
}

impl AssumptionPolicyTable {
    /// Load from path.
    pub fn load_from_path(
        path: impl AsRef<std::path::Path>,
    ) -> Result<Self, AssumptionPolicyError> {
        let p = path.as_ref();
        if !p.exists() {
            return Err(AssumptionPolicyError::NotFound(p.display().to_string()));
        }
        let yaml = std::fs::read_to_string(p)?;
        let raw: RawTable = serde_yml::from_str(&yaml)?;
        let mut entries = BTreeMap::new();
        for e in raw.entries {
            let key = (e.defect_class, e.privacy_class);
            if entries.insert(key, e.clone()).is_some() {
                return Err(AssumptionPolicyError::Duplicate(key.0, key.1));
            }
        }
        let mut rules = BTreeMap::new();
        for r in raw.policy_rules {
            if rules.insert(r.id.clone(), r.clone()).is_some() {
                return Err(AssumptionPolicyError::DuplicateRule(r.id));
            }
        }
        Ok(Self {
            version: raw.version,
            entries,
            rules,
        })
    }

    /// Lookup.
    pub fn lookup(&self, defect: DefectClass, privacy: PolicyPrivacyClass) -> Option<&PolicyEntry> {
        self.entries.get(&(defect, privacy))
    }

    /// Is blocking.
    pub fn is_blocking(&self, defect: DefectClass, privacy: PolicyPrivacyClass) -> bool {
        matches!(
            self.lookup(defect, privacy).map(|e| e.resolution),
            Some(ResolutionPolicy::Blocking)
        )
    }

    /// Iterator over every entry — used by tests + by v4 P4
    /// (`UnblockPath::Waiver`) authority routing.
    pub fn iter(&self) -> impl Iterator<Item = &PolicyEntry> {
        self.entries.values()
    }

    /// Registry membership check. Backs `PolicyRuleId::new`.
    /// Returns `false` when the table predates the current schema (empty `rules` map).
    pub fn has_rule(&self, id: &str) -> bool {
        self.rules.contains_key(id)
    }

    /// v3 §10.2 follow-up — look up a registry row. `None` when the
    /// id isn't registered.
    pub fn rule(&self, id: &str) -> Option<&PolicyRule> {
        self.rules.get(id)
    }

    /// v3 §10.2 follow-up — iterate every registered rule. Stable
    /// alphabetical order (BTreeMap).
    pub fn rules(&self) -> impl Iterator<Item = &PolicyRule> {
        self.rules.values()
    }
}
