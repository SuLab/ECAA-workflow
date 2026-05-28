//! V4 alignment validation × lifecycle promotion
//! grid loaded from `config/promotion-gate-policy.yaml`.
//!
//! Closes the §7 lifecycle ladder has seven states and §18 has
//! six validation classes. The mapping between them is the actual
//! promotion gate. Pre-v4-P3 the gate was hard-coded in
//! `composer_v4/policy_gate.rs`; v4 P3 lifts it into config so the
//! cross product `(lifecycle_state × validation_class)` is data, not
//! code. F19 forbids ad-hoc promotion logic — every promotion attempt
//! consults this table.
//!
//! Schema sidecar: `config/_promotion-gate-policy.schema.json`.
//!
//! `PromotionGatePolicy` is intentionally `Serialize + Deserialize`
//! so the loader can run at composition time and so emitted packages
//! can record which policy version was active. The companion types
//! (`PassingClassCounts`, `PromotionDecision`) are runtime-only — they
//! travel with the planner and don't ship in the IR.
//!
//! Wired into the planner via `PlanningContext.promotion_gate:
//! Option<Arc<PromotionGatePolicy>>` and consulted at every promotion
//! check by `composer_v4::policy_gate::consult_promotion_gate`. The
//! consult records a `VerifierDecision::PromotionGateConsulted` row to
//! the substrate so the lookup is replayable from
//! `runtime/verifier-decisions.jsonl`.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use ts_rs::TS;

use crate::workflow_contracts::lifecycle::LifecycleState;

/// Top-level policy shape. Mirrors the YAML schema.
///
/// `states` is keyed by the `LifecycleState::canonical_name()` snake-
/// case form so config edits don't have to track Rust enum variant
/// names.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, TS, schemars::JsonSchema)]
#[ts(export)]
pub struct PromotionGatePolicy {
    /// Version.
    pub version: String,
    /// States.
    pub states: BTreeMap<String, StateRequirements>,
}

/// Per-state evidence requirements. Each of the six §18 validation
/// classes has an independent requirement; `required_approvals` adds
/// the credential-class gate the production state needs.
#[derive(
    Debug, Clone, Serialize, Deserialize, PartialEq, Eq, TS, Default, schemars::JsonSchema,
)]
#[ts(export)]
pub struct StateRequirements {
    #[serde(default = "default_optional")]
    /// Contract.
    pub contract: ClassRequirement,
    #[serde(default = "default_optional")]
    /// Golden.
    pub golden: ClassRequirement,
    #[serde(default = "default_optional")]
    /// Metamorphic.
    pub metamorphic: ClassRequirement,
    #[serde(default = "default_optional")]
    /// Biological invariant.
    pub biological_invariant: ClassRequirement,
    #[serde(default = "default_optional")]
    /// Statistical sanity.
    pub statistical_sanity: ClassRequirement,
    #[serde(default = "default_optional")]
    /// Reproducibility.
    pub reproducibility: ClassRequirement,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    /// Notes.
    pub notes: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    /// Required approvals.
    pub required_approvals: Vec<RequiredApproval>,
}

/// Per-class requirement. Three shapes:
/// - `"required"` — at least 1 passing validator of this class.
/// - `"optional"` — recorded if present but not gating.
/// - `{ min_count: N }` — at least `N` passing validators.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, TS, schemars::JsonSchema)]
#[ts(export)]
#[serde(untagged)]
pub enum ClassRequirement {
    /// Tag variant.
    Tag(ClassRequirementTag),
    /// Variant.
    /// Field value.
    MinCount { min_count: u32 },
}

/// Tag-form of `ClassRequirement` (untagged enum branch).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, TS, schemars::JsonSchema)]
#[ts(export)]
#[serde(rename_all = "lowercase")]
pub enum ClassRequirementTag {
    /// Required variant.
    Required,
    /// Optional variant.
    Optional,
}

impl Default for ClassRequirement {
    fn default() -> Self {
        ClassRequirement::Tag(ClassRequirementTag::Optional)
    }
}

fn default_optional() -> ClassRequirement {
    ClassRequirement::Tag(ClassRequirementTag::Optional)
}

/// Required approval credential class — e.g. `domain_expert`,
/// `validation_engineer`. Production state typically demands both.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, TS, schemars::JsonSchema)]
#[ts(export)]
pub struct RequiredApproval {
    /// Approval class.
    pub approval_class: String,
}

/// Runtime counts of passing validators per §18 class. Built by the
/// planner from `TaskNode.evidence.passed_validators` (using each
/// validator's `id` to classify into one of the six buckets).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PassingClassCounts {
    /// Contract.
    pub contract: u32,
    /// Golden.
    pub golden: u32,
    /// Metamorphic.
    pub metamorphic: u32,
    /// Biological invariant.
    pub biological_invariant: u32,
    /// Statistical sanity.
    pub statistical_sanity: u32,
    /// Reproducibility.
    pub reproducibility: u32,
}

/// Outcome of consulting the promotion gate. `Allow` is the green
/// path; `Deny` enumerates every missing class + missing approval so
/// the SME's recovery UI surfaces the full action list at once.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PromotionDecision {
    /// Allow variant.
    Allow,
    /// Deny variant.
    Deny {
        /// Missing classes.
        missing_classes: Vec<String>,
        /// Missing approvals.
        missing_approvals: Vec<String>,
    },
}

impl PromotionDecision {
    /// Is allow.
    pub fn is_allow(&self) -> bool {
        matches!(self, PromotionDecision::Allow)
    }
}

/// Loader / decision API.
impl PromotionGatePolicy {
    /// Load + parse + wrap in `Arc` for cheap PlanningContext cloning.
    pub fn load_from_file(path: &Path) -> Result<Arc<Self>, PromotionGateError> {
        let raw = std::fs::read_to_string(path).map_err(|e| PromotionGateError::Io {
            path: path.display().to_string(),
            message: e.to_string(),
        })?;
        let p: PromotionGatePolicy =
            serde_yml::from_str(&raw).map_err(|e| PromotionGateError::Parse {
                message: e.to_string(),
            })?;
        Ok(Arc::new(p))
    }

    /// Consult the grid for a candidate `(target, counts, recorded_approvals)`.
    ///
    /// `Allow` iff every required class has at least one passing
    /// validator (or `min_count` is met) AND every `required_approvals`
    /// credential class appears in `recorded_approvals`.
    pub fn consult(
        &self,
        target: &LifecycleState,
        counts: &PassingClassCounts,
        recorded_approvals: &[String],
    ) -> PromotionDecision {
        let key = target.canonical_name().to_string();
        let req = match self.states.get(&key) {
            Some(r) => r,
            None => {
                return PromotionDecision::Deny {
                    missing_classes: vec!["unknown_target_state".to_string()],
                    missing_approvals: Vec::new(),
                };
            }
        };
        let mut missing = Vec::new();
        check_class("contract", &req.contract, counts.contract, &mut missing);
        check_class("golden", &req.golden, counts.golden, &mut missing);
        check_class(
            "metamorphic",
            &req.metamorphic,
            counts.metamorphic,
            &mut missing,
        );
        check_class(
            "biological_invariant",
            &req.biological_invariant,
            counts.biological_invariant,
            &mut missing,
        );
        check_class(
            "statistical_sanity",
            &req.statistical_sanity,
            counts.statistical_sanity,
            &mut missing,
        );
        check_class(
            "reproducibility",
            &req.reproducibility,
            counts.reproducibility,
            &mut missing,
        );

        let mut missing_approvals = Vec::new();
        for ra in &req.required_approvals {
            if !recorded_approvals.iter().any(|a| a == &ra.approval_class) {
                missing_approvals.push(ra.approval_class.clone());
            }
        }

        if missing.is_empty() && missing_approvals.is_empty() {
            PromotionDecision::Allow
        } else {
            PromotionDecision::Deny {
                missing_classes: missing,
                missing_approvals,
            }
        }
    }

    /// Enumerate the canonical required-class names for a target state.
    /// Used by `policy_gate.rs` to populate the substrate's
    /// `required_classes` field deterministically.
    pub fn required_class_names(&self, target: &LifecycleState) -> Vec<String> {
        let key = target.canonical_name().to_string();
        let Some(req) = self.states.get(&key) else {
            return Vec::new();
        };
        let mut names = Vec::new();
        for (name, req) in [
            ("contract", &req.contract),
            ("golden", &req.golden),
            ("metamorphic", &req.metamorphic),
            ("biological_invariant", &req.biological_invariant),
            ("statistical_sanity", &req.statistical_sanity),
            ("reproducibility", &req.reproducibility),
        ] {
            match req {
                ClassRequirement::Tag(ClassRequirementTag::Required) => {
                    names.push(name.to_string());
                }
                ClassRequirement::MinCount { .. } => {
                    names.push(name.to_string());
                }
                ClassRequirement::Tag(ClassRequirementTag::Optional) => {}
            }
        }
        names
    }
}

fn check_class(name: &str, req: &ClassRequirement, actual: u32, missing: &mut Vec<String>) {
    match req {
        ClassRequirement::Tag(ClassRequirementTag::Optional) => {}
        ClassRequirement::Tag(ClassRequirementTag::Required) => {
            if actual < 1 {
                missing.push(name.to_string());
            }
        }
        ClassRequirement::MinCount { min_count } => {
            if actual < *min_count {
                missing.push(format!("{name}@>={min_count}"));
            }
        }
    }
}

/// Typed loader/decision error.
#[derive(thiserror::Error, Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum PromotionGateError {
    #[error("io error reading {path}: {message}")]
    /// Variant.
    /// Field value.
    /// Field value.
    Io { path: String, message: String },
    #[error("parse error: {message}")]
    /// Variant.
    /// Field value.
    Parse { message: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn locally_validated_minimal() -> PromotionGatePolicy {
        let mut states = BTreeMap::new();
        states.insert(
            "locally_validated".into(),
            StateRequirements {
                contract: ClassRequirement::Tag(ClassRequirementTag::Required),
                golden: ClassRequirement::Tag(ClassRequirementTag::Required),
                metamorphic: ClassRequirement::MinCount { min_count: 1 },
                biological_invariant: ClassRequirement::MinCount { min_count: 1 },
                statistical_sanity: ClassRequirement::Tag(ClassRequirementTag::Optional),
                reproducibility: ClassRequirement::Tag(ClassRequirementTag::Optional),
                notes: "test".into(),
                required_approvals: Vec::new(),
            },
        );
        PromotionGatePolicy {
            version: "1.0.0".into(),
            states,
        }
    }

    #[test]
    fn consult_allow_when_all_requirements_met() {
        let policy = locally_validated_minimal();
        let counts = PassingClassCounts {
            contract: 1,
            golden: 1,
            metamorphic: 1,
            biological_invariant: 1,
            statistical_sanity: 0,
            reproducibility: 0,
        };
        let d = policy.consult(&LifecycleState::LocallyValidated, &counts, &[]);
        assert_eq!(d, PromotionDecision::Allow);
    }

    #[test]
    fn consult_deny_when_missing_class() {
        let policy = locally_validated_minimal();
        let counts = PassingClassCounts {
            contract: 1,
            golden: 1,
            metamorphic: 0, // missing
            biological_invariant: 1,
            statistical_sanity: 0,
            reproducibility: 0,
        };
        let d = policy.consult(&LifecycleState::LocallyValidated, &counts, &[]);
        match d {
            PromotionDecision::Deny {
                missing_classes, ..
            } => assert!(missing_classes.iter().any(|c| c.starts_with("metamorphic"))),
            other => panic!("expected Deny, got {other:?}"),
        }
    }

    #[test]
    fn consult_deny_when_unknown_target_state() {
        let policy = locally_validated_minimal();
        let counts = PassingClassCounts::default();
        let d = policy.consult(&LifecycleState::Production, &counts, &[]);
        match d {
            PromotionDecision::Deny {
                missing_classes, ..
            } => assert!(missing_classes.contains(&"unknown_target_state".to_string())),
            other => panic!("expected Deny, got {other:?}"),
        }
    }

    #[test]
    fn consult_deny_when_missing_approval() {
        let mut policy = locally_validated_minimal();
        policy
            .states
            .get_mut("locally_validated")
            .unwrap()
            .required_approvals
            .push(RequiredApproval {
                approval_class: "domain_expert".into(),
            });
        let counts = PassingClassCounts {
            contract: 1,
            golden: 1,
            metamorphic: 1,
            biological_invariant: 1,
            statistical_sanity: 0,
            reproducibility: 0,
        };
        let d = policy.consult(&LifecycleState::LocallyValidated, &counts, &[]);
        match d {
            PromotionDecision::Deny {
                missing_approvals, ..
            } => assert_eq!(missing_approvals, vec!["domain_expert".to_string()]),
            other => panic!("expected Deny, got {other:?}"),
        }
    }

    #[test]
    fn min_count_enforces_threshold() {
        let mut policy = locally_validated_minimal();
        policy
            .states
            .get_mut("locally_validated")
            .unwrap()
            .metamorphic = ClassRequirement::MinCount { min_count: 2 };
        let counts = PassingClassCounts {
            contract: 1,
            golden: 1,
            metamorphic: 1, // below threshold of 2
            biological_invariant: 1,
            statistical_sanity: 0,
            reproducibility: 0,
        };
        let d = policy.consult(&LifecycleState::LocallyValidated, &counts, &[]);
        match d {
            PromotionDecision::Deny {
                missing_classes, ..
            } => assert!(missing_classes.iter().any(|c| c == "metamorphic@>=2")),
            other => panic!("expected Deny, got {other:?}"),
        }
    }

    #[test]
    fn required_class_names_lists_only_gating_classes() {
        let policy = locally_validated_minimal();
        let names = policy.required_class_names(&LifecycleState::LocallyValidated);
        // contract + golden + metamorphic + biological_invariant are gating
        assert!(names.contains(&"contract".into()));
        assert!(names.contains(&"golden".into()));
        assert!(names.contains(&"metamorphic".into()));
        assert!(names.contains(&"biological_invariant".into()));
        assert!(!names.contains(&"statistical_sanity".into()));
        assert!(!names.contains(&"reproducibility".into()));
    }

    #[test]
    fn round_trips_through_yaml() {
        let policy = locally_validated_minimal();
        let yaml = serde_yml::to_string(&policy).unwrap();
        let back: PromotionGatePolicy = serde_yml::from_str(&yaml).unwrap();
        assert_eq!(policy, back);
    }
}
