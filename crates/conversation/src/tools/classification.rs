//! classifier-facing read tools.
//!
//! `classify_intake` runs the keyword classifier on free-form prose;
//! `get_classification_evidence` surfaces the stored classification's
//! evidence (organisms, data sources, methods detected). Both are
//! reads — neither mutates the session. `load_classifier` lives here
//! too since `intake::append_intake_prose` is the only other caller.
//!
//! # additive `goal` extraction
//!
//! `classify_intake`'s output is wrapped in [`ClassifyIntakeOutput`]
//! so the tool surface gains an optional `goal:` field carrying the
//! LLM's extracted [`GoalSpec`]. The deterministic classifier itself
//! never authors a goal (it's a keyword-only extractor); the slot is
//! populated by future LLM-mediated extraction paths or by callers
//! that have already inferred a goal from prose. The composer
//! (S6.11 / S7.2) reads this slot when the SME's intake commits to a
//! specific deliverable shape, falling back to archetype matching on
//! the keyword classifier's modality otherwise.

use crate::errors::{ToolError, ToolResult};
use crate::session::Session;
use ecaa_workflow_core::classify::{ClassificationResult, Classifier};
use ecaa_workflow_core::goal_spec::GoalSpec;
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Wrapper around [`ClassificationResult`] that the `classify_intake`
/// tool serializes. Flattens the keyword-classifier output verbatim so
/// existing schema consumers continue to read `modality`, `confidence`,
/// `organisms`, etc. at the top level; adds an optional `goal:` field
/// per plan §S6.4 for the LLM-extracted [`GoalSpec`].
///
/// `goal` is omitted from the serialized JSON when `None`, so existing
/// fixtures that snapshot `classify_intake` output without a goal block
/// stay byte-stable.
///
/// Not a ts-rs export — the wrapper is a runtime serialization shape
/// only. The UI talks to the tool surface through opaque
/// `serde_json::Value` blobs in `ToolCallRecord::result`; tightening
/// the schema contract is a Stage-7 follow-up.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub(super) struct ClassifyIntakeOutput {
    /// Deterministic keyword-classifier output. Flattened so the
    /// existing field surface (modality, confidence, organisms, etc.)
    /// stays at the top level of the serialized JSON.
    #[serde(flatten)]
    pub classification: ClassificationResult,

    /// Optional LLM-extracted goal block (plan §S6.1 / §S6.4). The
    /// keyword classifier never populates this — it's a slot for the
    /// LLM-mediated extraction path that lands alongside this wiring.
    /// Phase-2 fixtures that don't carry a goal serialize without
    /// the field thanks to `skip_serializing_if`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub goal: Option<GoalSpec>,
}

impl ClassifyIntakeOutput {
    /// Defensive sanitizer on `ClassifyIntakeOutput`.
    /// When the LLM populates `goal` with malformed EDAM IRIs
    /// (typo'd prefix, non-numeric id, `data:` namespace
    /// hallucination, etc.), drop the goal to None rather than
    /// retrying. The model rarely recovers on a second pass and
    /// content-mismatch retries burn budget without payoff. The
    /// composer downstream treats `goal: None` as "no constraint;
    /// route by archetype only," which is the correct fallback.
    ///
    /// Returns `Some(reason)` describing the dropped goal for
    /// logging / audit purposes; returns `None` when the goal
    /// already validates (no-op).
    pub(super) fn sanitize_goal_or_drop(&mut self) -> Option<String> {
        let goal = self.goal.take()?;
        if goal.is_well_formed() {
            self.goal = Some(goal);
            return None;
        }
        Some(format!(
            "dropped malformed LLM-extracted goal: edam_data='{}' edam_format={:?}",
            goal.edam_data, goal.edam_format
        ))
    }
}

pub(super) fn load_classifier(config_dir: &Path) -> Result<Classifier, ToolError> {
    let path = config_dir.join("modality-keywords.yaml");
    Classifier::load(&path).map_err(|e| ToolError::InternalError {
        reason: format!("loading classifier config: {}", e),
    })
}

pub(super) fn classify_intake(prose: &str, config_dir: &Path) -> ToolResult {
    if prose.trim().is_empty() {
        return ToolResult::err(ToolError::ValidationFailure {
            reason: "prose is empty".into(),
            valid_alternatives: vec![],
            hint: "Pass a non-empty description to classify_intake.".into(),
        });
    }
    match load_classifier(config_dir) {
        Ok(clf) => {
            let classification = clf.classify(prose);
            // Phase-2 ships the slot; deterministic classifier never
            // authors a goal. LLM-mediated callers populate `goal`
            // by deserializing a shaped `ClassifyIntakeOutput` from
            // their structured-output channel.
            let mut output = ClassifyIntakeOutput {
                classification,
                goal: None,
            };
            // Sanitize at the tool-output seam. No-op
            // today (`output.goal` is None on the deterministic
            // path), but wires the validation into the actual call
            // chain so when the LLM-extraction path lands and
            // populates `goal`, the same sanitizer runs without a
            // separate call site.
            if let Some(reason) = output.sanitize_goal_or_drop() {
                tracing::warn!(reason = %reason, "classify_intake sanitize_goal_or_drop triggered");
            }
            ToolResult::ok(serde_json::to_value(output).unwrap_or(serde_json::Value::Null))
        }
        Err(e) => ToolResult::err(e),
    }
}

pub(super) fn get_classification_evidence(session: &Session) -> ToolResult {
    let body = match &session.classification {
        Some(c) => serde_json::json!({
            "modality": c.modality,
            "confidence": c.confidence,
            "confidence_label": c.confidence_label,
            "organisms": c.organisms,
            "data_sources": c.data_sources,
            "methods_specified": c.methods_specified,
            "edam_topic": c.edam_topic,
            "edam_operation": c.edam_operation,
        }),
        None => serde_json::json!({
            "modality": null,
            "note": "no classification yet — call classify_intake or append_intake_prose first"
        }),
    };
    ToolResult::ok(body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ecaa_workflow_core::classify::ClassificationResult;

    fn empty_classification() -> ClassificationResult {
        ClassificationResult {
            modality: "single_cell_rnaseq".into(),
            taxonomy_path: String::new(),
            domain: String::new(),
            workflow_description: String::new(),
            edam_topic: String::new(),
            edam_operation: String::new(),
            confidence: 0.0,
            confidence_label: "low".into(),
            organisms: vec![],
            methods_specified: vec![],
            data_sources: vec![],
            intake_text: String::new(),
            goal: None,
            archetype_id: None,
            additional_modalities: vec![],
            tie_candidates: vec![],
        }
    }

    /// Well-formed LLM-extracted goal passes through
    /// the sanitizer untouched.
    #[test]
    fn sanitize_goal_keeps_well_formed_goal() {
        let mut output = ClassifyIntakeOutput {
            classification: empty_classification(),
            goal: Some(GoalSpec {
                edam_data: "data:3917".into(),
                edam_format: Some("format:3590".into()),
                modifiers: std::collections::BTreeMap::new(),
                source_prose: None,
                confidence: 0.85,
            }),
        };
        assert!(output.sanitize_goal_or_drop().is_none());
        assert!(output.goal.is_some());
    }

    /// Malformed `edam_data` (missing prefix) drops to
    /// None and the sanitizer returns a diagnostic.
    #[test]
    fn sanitize_goal_drops_malformed_edam_data() {
        let mut output = ClassifyIntakeOutput {
            classification: empty_classification(),
            goal: Some(GoalSpec {
                edam_data: "AnnData".into(), // missing `data:` prefix
                edam_format: Some("format:3590".into()),
                modifiers: std::collections::BTreeMap::new(),
                source_prose: None,
                confidence: 0.85,
            }),
        };
        let reason = output.sanitize_goal_or_drop().expect("goal dropped");
        assert!(
            reason.contains("AnnData"),
            "diagnostic mentions the bad IRI"
        );
        assert!(output.goal.is_none(), "goal cleared after drop");
    }

    /// Malformed `edam_format` (typo'd prefix) drops
    /// the entire goal even when `edam_data` is fine. We never
    /// partial-keep — the GoalSpec is an atomic unit.
    #[test]
    fn sanitize_goal_drops_malformed_edam_format() {
        let mut output = ClassifyIntakeOutput {
            classification: empty_classification(),
            goal: Some(GoalSpec {
                edam_data: "data:3917".into(),
                edam_format: Some("frmat:3590".into()), // typo
                modifiers: std::collections::BTreeMap::new(),
                source_prose: None,
                confidence: 0.85,
            }),
        };
        let reason = output.sanitize_goal_or_drop().expect("goal dropped");
        assert!(reason.contains("frmat:3590"));
        assert!(output.goal.is_none());
    }

    /// None goal is a no-op (no false-positive drop
    /// diagnostic).
    #[test]
    fn sanitize_goal_noop_on_none() {
        let mut output = ClassifyIntakeOutput {
            classification: empty_classification(),
            goal: None,
        };
        assert!(output.sanitize_goal_or_drop().is_none());
        assert!(output.goal.is_none());
    }

    /// `ecaax:` namespace IRIs are valid (per ADR
    /// 0004); the sanitizer accepts them.
    #[test]
    fn sanitize_goal_accepts_ecaax_namespace() {
        let mut output = ClassifyIntakeOutput {
            classification: empty_classification(),
            goal: Some(GoalSpec {
                edam_data: "ecaax:scrnaseq_annotation".into(),
                edam_format: None,
                modifiers: std::collections::BTreeMap::new(),
                source_prose: None,
                confidence: 0.7,
            }),
        };
        assert!(output.sanitize_goal_or_drop().is_none());
        assert!(output.goal.is_some());
    }
}
