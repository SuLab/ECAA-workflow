//! ED/CF rubric self-assessment. The system locates ITSELF in the
//! Extensibility-Dimension (ED) / Counterfactual-Floor (CF) design space,
//! emitted as a package sidecar. This LOCATES; it does NOT validate
//! (consistency ≠ validity).
//!
//! Deterministic: every field derives from already-emitted package facts
//! (tool-vocabulary size, atom/modality counts, gate strictness). No live
//! API, no timestamps, fixed-field structs only (no `HashMap`).

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Inputs the assessment derives from. All are deterministic package facts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssessmentInputs {
    /// Closed tool-vocabulary size (`Tool::COUNT`).
    pub tool_count: usize,
    /// Number of catalog atoms.
    pub atom_count: usize,
    /// Number of keyword-routable modalities.
    pub modality_count: usize,
    /// Number of `BlockerKind` variants.
    pub blocker_kind_count: usize,
    /// Whether the sandbox default is strict (network-deny, container-required).
    pub sandbox_default_strict: bool,
    /// Whether the gap→proposal→promotion pipeline is present.
    pub proposal_pipeline_present: bool,
    /// Whether LLM-assisted authoring (atom/renderer drafters) is present.
    pub llm_assisted_authoring_present: bool,
    /// Whether external-registry tiered import is present.
    pub external_import_tiers_present: bool,
    /// Whether schema-version migration is present.
    pub schema_migration_present: bool,
    /// Whether the emit_package confirmation latch is enforced.
    pub confirmation_latch_present: bool,
    /// Number of high-impact alone-in-turn tools (deterministic-gated).
    pub high_impact_tool_count: usize,
    /// Whether runtime claim verification is present.
    pub claim_verification_present: bool,
    /// Whether the audit-proof report is emitted.
    pub audit_proof_present: bool,
}

impl AssessmentInputs {
    /// Derive inputs from package/build facts. `atom_count` + `modality_count`
    /// come from the config registries; `tool_count` + `high_impact_tool_count`
    /// are supplied by the conversation-crate caller (core cannot depend on
    /// conversation). The presence flags are TRUE-by-construction for this
    /// build (the mechanisms ship in this binary); they are recorded
    /// explicitly so an ablation build that suppresses one reflects honestly.
    pub fn from_package_facts(
        atom_count: usize,
        modality_count: usize,
        tool_count: usize,
        high_impact_tool_count: usize,
    ) -> Self {
        use crate::ablation::{AblationFlag, AblationFlagExt};
        Self {
            tool_count,
            atom_count,
            modality_count,
            blocker_kind_count: {
                use strum::EnumCount;
                crate::blocker::BlockerKind::COUNT
            },
            sandbox_default_strict: true,
            proposal_pipeline_present: true,
            // Honest: drafters ship in the conversation crate. The core
            // emit path can't see whether a drafter fired, so this records
            // capability presence (the binary CAN draft), not per-package use.
            llm_assisted_authoring_present: true,
            external_import_tiers_present: true,
            schema_migration_present: true,
            confirmation_latch_present: true,
            high_impact_tool_count,
            claim_verification_present: true,
            audit_proof_present: !AblationFlag::AuditProof.is_active(),
        }
    }
}

/// One rubric axis with a 0.0–1.0 score and the human-readable mechanisms
/// that produced it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, schemars::JsonSchema)]
#[ts(export)]
pub struct RubricAxis {
    /// Score in [0.0, 1.0], deterministically derived.
    pub score: f64,
    /// Mechanisms counted toward this score (sorted, human-readable).
    pub mechanisms: Vec<String>,
}

/// The system's self-located ED/CF coordinates. Informational / warn-only —
/// it locates, it does not validate.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, schemars::JsonSchema)]
#[ts(export)]
#[non_exhaustive]
pub struct EdCfSelfAssessment {
    /// Schema version of this sidecar shape.
    pub schema_version: String,
    /// Extensibility Dimension — how many extension mechanisms are present.
    pub extensibility: RubricAxis,
    /// Counterfactual Floor — how strict the gates that prevent unsafe
    /// emission/execution are.
    pub counterfactual_floor: RubricAxis,
    /// Explicit disclaimer surfaced in the UI + report.
    pub disclaimer: String,
}

const DISCLAIMER: &str = "This report LOCATES the system in the ED/CF design space; it does NOT \
     validate the package. Consistency is not validity.";

impl EdCfSelfAssessment {
    /// Derive the assessment deterministically from package facts.
    pub fn from_inputs(i: &AssessmentInputs) -> Self {
        // ED: fraction of the five extension mechanisms present.
        let ed_mechs: Vec<(&str, bool)> = vec![
            ("gap_proposal_pipeline", i.proposal_pipeline_present),
            ("llm_assisted_authoring", i.llm_assisted_authoring_present),
            ("local_extension_graduation", i.proposal_pipeline_present),
            (
                "external_registry_import_tiers",
                i.external_import_tiers_present,
            ),
            ("schema_version_migration", i.schema_migration_present),
        ];
        let ed_present: Vec<String> = ed_mechs
            .iter()
            .filter(|(_, p)| *p)
            .map(|(n, _)| n.to_string())
            .collect();
        let extensibility = RubricAxis {
            score: ed_present.len() as f64 / ed_mechs.len() as f64,
            mechanisms: {
                let mut m = ed_present;
                m.sort();
                m
            },
        };
        // CF: fraction of the five gate-strictness mechanisms present.
        let cf_mechs: Vec<(&str, bool)> = vec![
            ("emit_confirmation_latch", i.confirmation_latch_present),
            (
                "high_impact_alone_in_turn_gating",
                i.high_impact_tool_count > 0,
            ),
            ("sandbox_default_strict", i.sandbox_default_strict),
            ("runtime_claim_verification", i.claim_verification_present),
            ("audit_proof_report", i.audit_proof_present),
        ];
        let cf_present: Vec<String> = cf_mechs
            .iter()
            .filter(|(_, p)| *p)
            .map(|(n, _)| n.to_string())
            .collect();
        let counterfactual_floor = RubricAxis {
            score: cf_present.len() as f64 / cf_mechs.len() as f64,
            mechanisms: {
                let mut m = cf_present;
                m.sort();
                m
            },
        };
        Self {
            schema_version: "0.1".to_string(),
            extensibility,
            counterfactual_floor,
            disclaimer: DISCLAIMER.to_string(),
        }
    }
}

/// Longitudinal ED/CF delta — diffs a child package's self-location against
/// its parent's across a lineage edge (amend/branch). Deterministic;
/// written as `runtime/ed-cf-delta.json` when lineage is set.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, schemars::JsonSchema)]
#[ts(export)]
#[non_exhaustive]
pub struct EdCfDelta {
    /// child.ED − parent.ED.
    pub extensibility_delta: f64,
    /// child.CF − parent.CF.
    pub counterfactual_floor_delta: f64,
    /// ED mechanisms present in child but not parent (sorted).
    pub gained_extensibility_mechanisms: Vec<String>,
    /// ED mechanisms present in parent but not child (sorted).
    pub lost_extensibility_mechanisms: Vec<String>,
    /// CF mechanisms present in child but not parent (sorted).
    pub gained_counterfactual_mechanisms: Vec<String>,
    /// CF mechanisms present in parent but not child (sorted).
    pub lost_counterfactual_mechanisms: Vec<String>,
}

impl EdCfDelta {
    /// Compute the delta `parent → child`. Pure; deterministic.
    pub fn between(parent: &EdCfSelfAssessment, child: &EdCfSelfAssessment) -> Self {
        fn gained(p: &[String], c: &[String]) -> Vec<String> {
            let mut v: Vec<String> = c.iter().filter(|m| !p.contains(m)).cloned().collect();
            v.sort();
            v
        }
        Self {
            extensibility_delta: child.extensibility.score - parent.extensibility.score,
            counterfactual_floor_delta: child.counterfactual_floor.score
                - parent.counterfactual_floor.score,
            gained_extensibility_mechanisms: gained(
                &parent.extensibility.mechanisms,
                &child.extensibility.mechanisms,
            ),
            lost_extensibility_mechanisms: gained(
                &child.extensibility.mechanisms,
                &parent.extensibility.mechanisms,
            ),
            gained_counterfactual_mechanisms: gained(
                &parent.counterfactual_floor.mechanisms,
                &child.counterfactual_floor.mechanisms,
            ),
            lost_counterfactual_mechanisms: gained(
                &child.counterfactual_floor.mechanisms,
                &parent.counterfactual_floor.mechanisms,
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn full_inputs() -> AssessmentInputs {
        AssessmentInputs {
            tool_count: 22,
            atom_count: 101,
            modality_count: 23,
            blocker_kind_count: 49,
            sandbox_default_strict: true,
            proposal_pipeline_present: true,
            llm_assisted_authoring_present: true,
            external_import_tiers_present: true,
            schema_migration_present: true,
            confirmation_latch_present: true,
            high_impact_tool_count: 6,
            claim_verification_present: true,
            audit_proof_present: true,
        }
    }

    /// The rubric fixture's `atom_count` / `modality_count` must track the
    /// live config registries so the self-assessment never reports stale
    /// architecture facts. These pins mirror the runtime count-baseline
    /// gates in `crates/core/tests/modality_count_baseline.rs`,
    /// `crates/core/tests/archetype_count_baseline.rs`, and
    /// `crates/core/tests/atom_registry/atom_count_baseline.rs`. Bump them
    /// in the same change that adds/removes an atom or modality manifest.
    #[test]
    fn full_inputs_fixture_matches_live_baselines() {
        let f = full_inputs();
        assert_eq!(
            f.atom_count, 101,
            "rubric fixture atom_count is stale (WS-D2)"
        );
        assert_eq!(
            f.modality_count, 23,
            "rubric fixture modality_count is stale (WS-D2)"
        );
    }

    #[test]
    fn full_inputs_score_max() {
        let a = EdCfSelfAssessment::from_inputs(&full_inputs());
        assert!((a.extensibility.score - 1.0).abs() < 1e-9);
        assert!((a.counterfactual_floor.score - 1.0).abs() < 1e-9);
    }

    #[test]
    fn deterministic_byte_stable() {
        let a = serde_json::to_string(&EdCfSelfAssessment::from_inputs(&full_inputs())).unwrap();
        let b = serde_json::to_string(&EdCfSelfAssessment::from_inputs(&full_inputs())).unwrap();
        assert_eq!(a, b, "assessment must be byte-stable for fixed inputs");
    }

    #[test]
    fn dropping_a_mechanism_lowers_the_axis() {
        let mut i = full_inputs();
        i.audit_proof_present = false;
        let a = EdCfSelfAssessment::from_inputs(&i);
        assert!(a.counterfactual_floor.score < 1.0);
        assert!(!a
            .counterfactual_floor
            .mechanisms
            .contains(&"audit_proof_report".to_string()));
    }

    #[test]
    fn disclaimer_states_consistency_is_not_validity() {
        let a = EdCfSelfAssessment::from_inputs(&full_inputs());
        assert!(a.disclaimer.contains("not validity") || a.disclaimer.contains("does NOT"));
    }

    #[test]
    fn from_package_facts_derives_present_mechanisms() {
        let inputs = AssessmentInputs::from_package_facts(97, 23, 22, 6);
        // blocker_kind_count comes from core's BlockerKind enum count.
        assert!(inputs.blocker_kind_count > 0);
        // The architecture's standing mechanisms are present by construction.
        assert!(inputs.proposal_pipeline_present);
        assert!(inputs.audit_proof_present);
        assert!(inputs.confirmation_latch_present);
    }

    #[test]
    fn assessment_round_trips_its_own_json_schema() {
        use jsonschema::JSONSchema;
        let schema_value = serde_json::to_value(schemars::schema_for!(EdCfSelfAssessment)).unwrap();
        let compiled = JSONSchema::compile(&schema_value).expect("schema compiles");
        let instance = serde_json::to_value(EdCfSelfAssessment::from_inputs(
            &AssessmentInputs::from_package_facts(97, 23, 22, 6),
        ))
        .unwrap();
        assert!(
            compiled.validate(&instance).is_ok(),
            "emitted assessment must validate against its own derived JSON Schema"
        );
    }

    #[test]
    fn ed_cf_delta_reflects_an_ed_bump_cf_unchanged() {
        let parent = EdCfSelfAssessment::from_inputs(&{
            let mut i = full_inputs();
            i.llm_assisted_authoring_present = false; // parent had no drafter
            i
        });
        let child = EdCfSelfAssessment::from_inputs(&full_inputs()); // child gained it
        let delta = EdCfDelta::between(&parent, &child);
        assert!(delta.extensibility_delta > 0.0, "ED should rise");
        assert!(
            (delta.counterfactual_floor_delta - 0.0).abs() < 1e-9,
            "CF unchanged"
        );
        assert!(delta
            .gained_extensibility_mechanisms
            .contains(&"llm_assisted_authoring".to_string()));
    }

    #[test]
    fn ed_cf_delta_idempotent_on_identical() {
        let a = EdCfSelfAssessment::from_inputs(&full_inputs());
        let d = EdCfDelta::between(&a, &a);
        assert!((d.extensibility_delta - 0.0).abs() < 1e-9);
        assert!((d.counterfactual_floor_delta - 0.0).abs() < 1e-9);
        assert!(d.gained_extensibility_mechanisms.is_empty());
    }
}
