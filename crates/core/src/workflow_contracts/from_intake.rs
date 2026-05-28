//! Conversion from legacy intake types to `WorkflowIntent`.
//!
//! This module is the *typed bridge* between the existing intake
//! path (classification → facts → goal) and the proof-carrying
//! composer's intent surface. It satisfies three contracts:
//!
//! 1. The session persists `workflow_intent: Option<WorkflowIntent>`
//!    (lives in `crates/conversation`).
//! 2. `GoalSpec`, `IntakeFacts`, classifier modality fields, and
//!    structured capture values are derivable from / linked to
//!    `WorkflowIntent`.
//! 3. Composer paths can read either the legacy types or the new
//!    intent uniformly through this converter.
//!
//! No call site mutates intent through this converter — it
//! produces a fresh `WorkflowIntent` from the current legacy
//! shape. The conversation crate's intake tools materialize one
//! `WorkflowIntent` per session and cache the legacy types as
//! derived data.

use std::collections::BTreeMap;

use crate::classify::ClassificationResult;
use crate::goal_spec::GoalSpec;
use crate::intake_facts::IntakeFacts;

use super::data_product::PrivacyClass;
use super::workflow_intent::{
    ConstraintsBlock, DesiredOutput, ExecutionPreference, PrivacyBlock, UncertaintyEntry,
    UserExplanationStyle, WorkflowIntent,
};

impl WorkflowIntent {
    /// Build a `WorkflowIntent` from the current legacy intake
    /// shape. Bootstraps session state from existing
    /// classification output.
    pub fn from_legacy(
        session_id: &str,
        classification: &ClassificationResult,
        facts: &IntakeFacts,
        goal: Option<&GoalSpec>,
    ) -> Self {
        let mut legacy: BTreeMap<String, serde_json::Value> = BTreeMap::new();
        legacy.insert(
            "modality".into(),
            serde_json::Value::String(facts.modality.clone()),
        );
        legacy.insert(
            "project_class".into(),
            serde_json::to_value(facts.project_class).unwrap_or(serde_json::Value::Null),
        );
        if let Some(taxon) = facts.organism_taxon_id {
            legacy.insert("organism_taxon_id".into(), serde_json::json!(taxon));
        }
        if let Some(name) = &facts.organism_name {
            legacy.insert("organism_name".into(), serde_json::json!(name));
        }
        if !facts.methods.is_empty() {
            legacy.insert("methods".into(), serde_json::json!(facts.methods));
        }
        if let Some(c) = facts.sample_count {
            legacy.insert("sample_count".into(), serde_json::json!(c));
        }
        if let Some(c) = facts.coverage_depth {
            legacy.insert("coverage_depth".into(), serde_json::json!(c));
        }
        if let Some(c) = facts.cell_count {
            legacy.insert("cell_count".into(), serde_json::json!(c));
        }
        if let Some(c) = facts.database_size_gb {
            legacy.insert("database_size_gb".into(), serde_json::json!(c));
        }
        if !facts.pinned_accessions.is_empty() {
            legacy.insert(
                "pinned_accessions".into(),
                serde_json::to_value(&facts.pinned_accessions).unwrap_or(serde_json::Value::Null),
            );
        }
        if !facts.pinned_reference_bundles.is_empty() {
            legacy.insert(
                "pinned_reference_bundles".into(),
                serde_json::to_value(&facts.pinned_reference_bundles)
                    .unwrap_or(serde_json::Value::Null),
            );
        }
        legacy.insert(
            "classification_confidence".into(),
            serde_json::json!(classification.confidence),
        );

        let desired_outputs = goal
            .map(|g| {
                vec![DesiredOutput {
                    label: g
                        .source_prose
                        .clone()
                        .unwrap_or_else(|| g.edam_data.clone()),
                    edam_data: Some(g.edam_data.clone()),
                    edam_format: g.edam_format.clone(),
                    human_readable: false,
                }]
            })
            .unwrap_or_default();

        Self {
            id: session_id.to_string(),
            schema_version: crate::migration::current_workflow_intent_version(),
            goal: goal
                .and_then(|g| g.source_prose.clone())
                .unwrap_or_else(|| classification.modality.clone()),
            modality: Some(classification.modality.clone()),
            project_class: Some(facts.project_class.as_str().to_string()),
            available_data: Vec::new(),
            desired_outputs,
            constraints: ConstraintsBlock::default(),
            // Organisms with low classification confidence become
            // uncertainty entries; the composer's ranking penalizes
            // paths whose inferred organism didn't score above the
            // confidence floor today.
            uncertainties: if classification.confidence
                < crate::classify_gates::CONFIDENCE_GATE_MEDIUM
            {
                vec![UncertaintyEntry {
                    topic: "modality".into(),
                    statement: format!(
                        "Modality {} inferred with low confidence ({:.2})",
                        classification.modality, classification.confidence
                    ),
                    risk: super::evidence::RiskClass::Low,
                }]
            } else {
                vec![]
            },
            privacy: PrivacyBlock {
                default_class: PrivacyClass::Public,
                hipaa: false,
                cfr_part11: false,
                regulatory_context: vec![],
            },
            execution_preferences: vec![ExecutionPreference {
                key: "preferred_executor".into(),
                value: "local".into(),
            }],
            explanation_style: UserExplanationStyle::DomainFriendly,
            legacy_intake_facts: legacy,
            // Clinical projects populate this from the intake form;
            // non-clinical intake never carries it, so the lift
            // defaults to `None`.
            sample_cohort: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_classification() -> ClassificationResult {
        ClassificationResult {
            modality: "bulk_rnaseq".into(),
            confidence: 0.85,
            ..Default::default()
        }
    }

    fn fake_facts() -> IntakeFacts {
        IntakeFacts {
            modality: "bulk_rnaseq".into(),
            project_class: Default::default(),
            organism_taxon_id: Some(9606),
            organism_name: Some("Homo sapiens".into()),
            methods: vec![],
            sample_count: Some(12),
            coverage_depth: Some(30),
            cell_count: None,
            database_size_gb: None,
            pinned_accessions: vec![],
            pinned_reference_bundles: vec![],
            literature_review_requested: false,
            excluded_atoms: Vec::new(),
        }
    }

    fn fake_goal() -> GoalSpec {
        GoalSpec {
            edam_data: "data:0951".into(),
            edam_format: Some("format:3475".into()),
            modifiers: BTreeMap::new(),
            source_prose: Some("DE table comparing treated vs control".into()),
            confidence: 0.9,
        }
    }

    #[test]
    fn from_legacy_round_trips() {
        let intent = WorkflowIntent::from_legacy(
            "s_test",
            &fake_classification(),
            &fake_facts(),
            Some(&fake_goal()),
        );
        let json = serde_json::to_string(&intent).unwrap();
        let back: WorkflowIntent = serde_json::from_str(&json).unwrap();
        assert_eq!(intent, back);
    }

    #[test]
    fn from_legacy_preserves_id_and_schema() {
        let intent = WorkflowIntent::from_legacy(
            "s_test",
            &fake_classification(),
            &fake_facts(),
            Some(&fake_goal()),
        );
        assert_eq!(intent.id, "s_test");
        assert_eq!(intent.schema_version, semver::Version::new(1, 0, 0));
        assert_eq!(intent.modality.as_deref(), Some("bulk_rnaseq"));
    }

    #[test]
    fn from_legacy_lifts_goal_into_desired_outputs() {
        let intent = WorkflowIntent::from_legacy(
            "s_test",
            &fake_classification(),
            &fake_facts(),
            Some(&fake_goal()),
        );
        assert_eq!(intent.desired_outputs.len(), 1);
        assert_eq!(
            intent.desired_outputs[0].edam_data.as_deref(),
            Some("data:0951")
        );
        assert_eq!(
            intent.desired_outputs[0].label,
            "DE table comparing treated vs control"
        );
    }

    #[test]
    fn from_legacy_carries_facts_into_legacy_block() {
        let intent =
            WorkflowIntent::from_legacy("s_test", &fake_classification(), &fake_facts(), None);
        let l = &intent.legacy_intake_facts;
        assert_eq!(l.get("modality"), Some(&serde_json::json!("bulk_rnaseq")));
        assert_eq!(l.get("organism_taxon_id"), Some(&serde_json::json!(9606)));
        assert_eq!(
            l.get("organism_name"),
            Some(&serde_json::json!("Homo sapiens"))
        );
        assert_eq!(l.get("sample_count"), Some(&serde_json::json!(12)));
    }

    #[test]
    fn from_legacy_without_goal_uses_modality_as_goal() {
        let intent =
            WorkflowIntent::from_legacy("s_test", &fake_classification(), &fake_facts(), None);
        assert_eq!(intent.goal, "bulk_rnaseq");
        assert!(intent.desired_outputs.is_empty());
    }
}
