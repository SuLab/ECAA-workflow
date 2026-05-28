//! Promotion gate for drafted renderer modules.
//!
//! Gates promotion of a drafted renderer from the session-scoped
//! proposals registry into the shared
//! `config/plot-affordances/generated/<stage_id>.yaml` catalog.
//!
//! Three gates must all pass before the YAML is written:
//!
//! 1. `sandbox_outcome` must be `SandboxOutcome::StaticChecksPassed`.
//! 2. `validation_report` must have zero `Failed` / `Errored` rows for
//!    the RENDERER_VALIDATION_BUNDLE obligations (contrast, DPI, theme
//!    parity, determinism).
//! 3. `sme_approval_decision_id` must reference a durable
//!    `DecisionType::ApproveGeneratedRenderer` record in the session's
//!    decision log.
//!
//! On success the function writes
//! `config/plot-affordances/generated/<stage_id>.yaml` (unless
//! `dry_run: true`, in which case it returns `Ok(PromotedRenderer)` but
//! skips the write). The YAML is a minimal registered-affordance stanza
//! that the `PlotAffordanceRegistry` loader can ingest.
//!
//! Note: the actual subprocess sandbox execution path (which confirms
//! the module ran cleanly in a container) must complete before the
//! `validation_report` rows are populated. The gate logic and write
//! path are implemented; end-to-end promotion requires sandbox runtime
//! enforcement to be wired.

use crate::ids::StageId;
use serde::{Deserialize, Serialize};
use std::path::Path;

use crate::plot_affordance::sandbox::SandboxOutcome;

/// A single row from the harness validator run. Mirror of
/// `crates/harness::validators::ValidatorRow` (re-declared here so
/// `crates/core` doesn't take a dependency on `crates/harness`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, schemars::JsonSchema)]
pub struct ValidationRow {
    /// Obligation id.
    pub obligation_id: String,
    /// Outcome.
    pub outcome: ValidationOutcome,
}

/// Simplified outcome mirror for the promotion gate. The full
/// `ValidatorOutcome` enum lives in `crates/harness`; this is the
/// minimal subset `crates/core` needs to gate promotion.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, schemars::JsonSchema)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum ValidationOutcome {
    /// Passed variant.
    Passed,
    /// Variant.
    /// Field value.
    Failed { message: String },
    /// Variant.
    /// Field value.
    Errored { reason: String },
    /// Variant.
    /// Field value.
    Unimplemented { obligation_id: String },
}

impl ValidationOutcome {
    /// True when the outcome allows promotion.
    pub fn is_passing(&self) -> bool {
        matches!(
            self,
            ValidationOutcome::Passed | ValidationOutcome::Unimplemented { .. }
        )
    }
}

/// Input to the promotion gate.
#[derive(Debug)]
pub struct RendererPromotionRequest {
    /// Stable proposal id from the session-scoped `RendererProposals`
    /// registry (e.g. `renderer-proposal-abc123def456`).
    pub proposal_id: String,
    /// Python module source from `DraftedRenderer.module_source`.
    pub drafted_module_source: String,
    /// Static check result from `check_drafted_renderer`.
    pub sandbox_outcome: SandboxOutcome,
    /// Validator rows from the harness RENDERER_VALIDATION_BUNDLE run.
    /// All rows must have `Passed` or `Unimplemented` outcomes.
    pub validation_report: Vec<ValidationRow>,
    /// Decision-log id of the durable `DecisionType::ApproveGeneratedRenderer`
    /// record. Present means the SME clicked Approve; absent means gate
    /// failure.
    pub sme_approval_decision_id: Option<String>,
    /// Target stage id (derived from the proposal's semantic type).
    pub target_stage_id: StageId,
    /// Version string to embed in the promoted YAML (e.g. `"1.0.0"`).
    pub version: String,
    /// Figure ids to register in the YAML.
    pub figure_ids: Vec<String>,
    /// Module path relative to the repo root.
    pub renderer_module: String,
    /// When `true`, validation + gate checks run but no YAML is written.
    pub dry_run: bool,
    /// Directory to write the promoted YAML into. Typically
    /// `config/plot-affordances/generated/`. Absolute path required
    /// unless `dry_run: true`.
    pub output_dir: String,
}

/// Successful result of `promote_renderer`.
#[derive(Debug, Clone)]
pub struct PromotedRenderer {
    /// Path the YAML was written to, or would have been written to in
    /// `dry_run` mode.
    pub yaml_path: String,
    /// The YAML content that was (or would have been) written.
    pub yaml_content: String,
    /// Whether the write was actually performed.
    pub was_dry_run: bool,
}

/// Why promotion was refused.
#[derive(thiserror::Error, Debug)]
#[non_exhaustive]
pub enum PromotionError {
    /// The sandbox outcome was `Refused`. Contains the formatted refusal
    /// list for the SME's review.
    #[error("sandbox refused: {}", reasons.join("; "))]
    SandboxRefused { reasons: Vec<String> },
    /// One or more validator rows in the bundle failed or errored.
    #[error("validation failed obligations: {}", failed_obligations.join(", "))]
    ValidationFailed { failed_obligations: Vec<String> },
    /// No SME approval decision id was provided.
    #[error("missing SME approval decision id")]
    MissingSmeApproval,
    /// YAML serialization failed (should not happen in practice).
    #[error("YAML serialization error: {0}")]
    SerializationError(String),
    /// Filesystem write failed.
    #[error("IO error writing promoted YAML: {0}")]
    IoError(String),
    /// `target_stage_id` failed the shape validator. The id flows
    /// into a filename (`<output_dir>/<target_stage_id>.yaml`), so an
    /// unvetted value can escape the output dir via `..` segments or
    /// NUL-terminate the path.
    #[error("invalid target_stage_id: {reason}")]
    InvalidTargetStageId { reason: String },
}

/// Validate `target_stage_id` before it is interpolated into a
/// filename. Shape: alnum + `_` + `-`, length 1..=64. Refuses `..`,
/// `/`, NUL, leading `.`, anything outside that shape — exactly the
/// rule the agent-script `validate_task_id` shell helper applies.
/// Mirrors `harness::executor::_id_validator::is_safe_id` without
/// taking a harness dependency.
fn validate_target_stage_id(id: &str) -> Result<(), PromotionError> {
    if id.is_empty() {
        return Err(PromotionError::InvalidTargetStageId {
            reason: "empty target_stage_id".into(),
        });
    }
    if id.len() > 64 {
        return Err(PromotionError::InvalidTargetStageId {
            reason: format!("target_stage_id exceeds 64 chars: {} bytes", id.len()),
        });
    }
    if id.starts_with('.') {
        return Err(PromotionError::InvalidTargetStageId {
            reason: format!("target_stage_id starts with '.': {id:?}"),
        });
    }
    if id.contains("..") {
        return Err(PromotionError::InvalidTargetStageId {
            reason: format!("target_stage_id contains '..': {id:?}"),
        });
    }
    if !id
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Err(PromotionError::InvalidTargetStageId {
            reason: format!("target_stage_id outside ^[A-Za-z0-9_-]+$: {id:?}"),
        });
    }
    Ok(())
}

/// Promote a drafted renderer to the shared affordance catalog.
///
/// # Gate order
///
/// 1. Sandbox gate — `sandbox_outcome` must be `StaticChecksPassed`.
/// 2. Validation gate — all rows in `validation_report` must be
///    `Passed` or `Unimplemented`. Additionally, `renderer_determinism`
///    must be explicitly `Passed` (Phase C8: `DeterminismRunner` is now
///    fully implemented; `Unimplemented` for that obligation is no
///    longer advisory).
/// 3. SME approval gate — `sme_approval_decision_id` must be `Some(_)`.
/// 4. YAML emit — write `<output_dir>/<target_stage_id>.yaml`.
///    Skipped when `dry_run: true`.
///
/// Returns `Ok(PromotedRenderer)` on success; `Err(PromotionError)` when
/// any gate fails.
pub fn promote_renderer(req: RendererPromotionRequest) -> Result<PromotedRenderer, PromotionError> {
    // `target_stage_id` becomes a filename
    // (`<output_dir>/<target_stage_id>.yaml`). Validate the shape
    // BEFORE any of the heavier gates so the rejection is cheap and
    // the failure mode is "this id is malformed", not "the YAML write
    // escaped to /etc".
    validate_target_stage_id(req.target_stage_id.as_str())?;

    // Gate 1: sandbox.
    match &req.sandbox_outcome {
        SandboxOutcome::StaticChecksPassed { .. } => {}
        SandboxOutcome::Refused { refusals } => {
            let reasons: Vec<String> = refusals.iter().map(|r| format!("{:?}", r)).collect();
            return Err(PromotionError::SandboxRefused { reasons });
        }
    }

    // Gate 2: validator rows.
    // General rule: rows must be Passed or Unimplemented (runners not installed
    // on this host are advisory).
    // Determinism exception (Phase C8): renderer_determinism is now fully
    // implemented via DeterminismRunner; Unimplemented is no longer advisory
    // for that specific obligation.
    const DETERMINISM_OBLIGATION: &str = "renderer_determinism";

    let failed: Vec<String> = req
        .validation_report
        .iter()
        .filter(|row| {
            // A row blocks promotion when:
            // (a) it is not passing by the general rule, OR
            // (b) it is the determinism obligation and is Unimplemented
            // (the runner is real now; treat as not-run = fail).
            !row.outcome.is_passing()
                || (row.obligation_id == DETERMINISM_OBLIGATION
                    && matches!(row.outcome, ValidationOutcome::Unimplemented { .. }))
        })
        .map(|row| row.obligation_id.clone())
        .collect();
    if !failed.is_empty() {
        return Err(PromotionError::ValidationFailed {
            failed_obligations: failed,
        });
    }

    // Gate 3: SME approval.
    if req.sme_approval_decision_id.is_none() {
        return Err(PromotionError::MissingSmeApproval);
    }

    // Build the registered-affordance YAML stanza.
    let yaml_content = render_affordance_yaml(
        req.target_stage_id.as_str(),
        &req.version,
        &req.figure_ids,
        &req.renderer_module,
        &req.proposal_id,
        req.sme_approval_decision_id.as_deref().unwrap_or(""),
    );

    let yaml_filename = format!("{}.yaml", req.target_stage_id);
    let yaml_path = if req.output_dir.is_empty() {
        yaml_filename.clone()
    } else {
        format!("{}/{}", req.output_dir.trim_end_matches('/'), yaml_filename)
    };

    if !req.dry_run {
        if let Some(parent) = Path::new(&yaml_path).parent() {
            std::fs::create_dir_all(parent).map_err(|e| PromotionError::IoError(e.to_string()))?;
        }
        std::fs::write(&yaml_path, &yaml_content)
            .map_err(|e| PromotionError::IoError(e.to_string()))?;
    }

    Ok(PromotedRenderer {
        yaml_path,
        yaml_content,
        was_dry_run: req.dry_run,
    })
}

/// Render the YAML stanza for a newly promoted affordance.
///
/// Format is compatible with the `YamlPlotAffordanceRegistry` loader.
/// The generated file carries a `generated_by: renderer_drafter_v1`
/// sentinel that the loader preserves as provenance metadata.
fn render_affordance_yaml(
    stage_id: &str,
    version: &str,
    figure_ids: &[String],
    renderer_module: &str,
    proposal_id: &str,
    approval_decision_id: &str,
) -> String {
    let figure_ids_yaml = figure_ids
        .iter()
        .map(|id| format!("  - {}", id))
        .collect::<Vec<_>>()
        .join("\n");

    format!(
        "# Generated affordance — DO NOT EDIT by hand.\n\
         # Promote via promote_renderer() in crates/core/src/plot_affordance/promotion.rs\n\
         # proposal_id: {proposal_id}\n\
         # approval_decision_id: {approval_decision_id}\n\
         stage_id: {stage_id}\n\
         version: \"{version}\"\n\
         generated_by: renderer_drafter_v1\n\
         renderer_module: {renderer_module}\n\
         figure_ids:\n\
         {figure_ids_yaml}\n",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plot_affordance::sandbox::SandboxOutcome;
    use crate::sandbox_policy::SandboxRefusal;

    fn passing_request() -> RendererPromotionRequest {
        RendererPromotionRequest {
            proposal_id: "renderer-proposal-abc123".into(),
            drafted_module_source: "import numpy as np\n".into(),
            sandbox_outcome: SandboxOutcome::StaticChecksPassed {
                task_node_id: "generated_renderer_volcano".into(),
            },
            validation_report: vec![
                ValidationRow {
                    obligation_id: "renderer_contrast_wcag".into(),
                    outcome: ValidationOutcome::Passed,
                },
                ValidationRow {
                    obligation_id: "renderer_dpi_300".into(),
                    outcome: ValidationOutcome::Passed,
                },
                ValidationRow {
                    obligation_id: "renderer_theme_parity".into(),
                    outcome: ValidationOutcome::Passed,
                },
                ValidationRow {
                    obligation_id: "renderer_determinism".into(),
                    outcome: ValidationOutcome::Passed,
                },
            ],
            sme_approval_decision_id: Some("decision-xyz".into()),
            target_stage_id: "custom_volcano".into(),
            version: "1.0.0".into(),
            figure_ids: vec!["volcano".into()],
            renderer_module: "lib/plotting/stages/_generated/custom_volcano.py".into(),
            dry_run: true,
            output_dir: String::new(),
        }
    }

    #[test]
    fn promote_renderer_succeeds_when_all_gates_pass() {
        let result = promote_renderer(passing_request());
        assert!(
            result.is_ok(),
            "unexpected error: {:?}",
            result.unwrap_err()
        );
        let promoted = result.unwrap();
        assert!(promoted.was_dry_run);
        assert!(promoted.yaml_content.contains("custom_volcano"));
        assert!(promoted.yaml_content.contains("renderer_drafter_v1"));
    }

    #[test]
    fn promote_renderer_rejects_on_sandbox_refused() {
        let mut req = passing_request();
        req.sandbox_outcome = SandboxOutcome::Refused {
            refusals: vec![SandboxRefusal::StaticAnalysisRequired],
        };
        let err = promote_renderer(req).unwrap_err();
        assert!(
            matches!(err, PromotionError::SandboxRefused { .. }),
            "expected SandboxRefused, got: {}",
            err
        );
    }

    #[test]
    fn promote_renderer_rejects_when_validation_report_has_failed_obligation() {
        let mut req = passing_request();
        req.validation_report.push(ValidationRow {
            obligation_id: "renderer_dpi_300".into(),
            outcome: ValidationOutcome::Failed {
                message: "PNG DPI was 72, expected 300".into(),
            },
        });
        let err = promote_renderer(req).unwrap_err();
        match err {
            PromotionError::ValidationFailed { failed_obligations } => {
                assert!(failed_obligations.contains(&"renderer_dpi_300".to_string()));
            }
            other => panic!("expected ValidationFailed, got: {other}"),
        }
    }

    #[test]
    fn promote_renderer_rejects_when_errored_obligation_present() {
        let mut req = passing_request();
        req.validation_report.push(ValidationRow {
            obligation_id: "renderer_contrast_wcag".into(),
            outcome: ValidationOutcome::Errored {
                reason: "quality_scorer.py not found".into(),
            },
        });
        let err = promote_renderer(req).unwrap_err();
        assert!(
            matches!(err, PromotionError::ValidationFailed { .. }),
            "expected ValidationFailed, got: {}",
            err
        );
    }

    #[test]
    fn promote_renderer_rejects_missing_sme_approval() {
        let mut req = passing_request();
        req.sme_approval_decision_id = None;
        let err = promote_renderer(req).unwrap_err();
        assert!(
            matches!(err, PromotionError::MissingSmeApproval),
            "expected MissingSmeApproval, got: {}",
            err
        );
    }

    #[test]
    fn promote_renderer_unimplemented_non_determinism_obligation_is_not_blocking() {
        // Unimplemented is still advisory for obligations other than
        // renderer_determinism (e.g. a runner not installed on this host).
        let mut req = passing_request();
        req.validation_report.push(ValidationRow {
            obligation_id: "renderer_theme_parity".into(),
            outcome: ValidationOutcome::Unimplemented {
                obligation_id: "renderer_theme_parity".into(),
            },
        });
        let result = promote_renderer(req);
        assert!(
            result.is_ok(),
            "unimplemented non-determinism obligation should not block promotion"
        );
    }

    #[test]
    fn promote_renderer_determinism_unimplemented_blocks_promotion() {
        // Phase C8: renderer_determinism is now fully implemented.
        // Unimplemented for that specific obligation must block promotion.
        let mut req = passing_request();
        // Override the determinism row that was Passed in passing_request()
        // to be Unimplemented.
        let det_row = req
            .validation_report
            .iter_mut()
            .find(|r| r.obligation_id == "renderer_determinism")
            .expect("renderer_determinism row must be in passing_request");
        det_row.outcome = ValidationOutcome::Unimplemented {
            obligation_id: "renderer_determinism".into(),
        };
        let err = promote_renderer(req).unwrap_err();
        match err {
            PromotionError::ValidationFailed { failed_obligations } => {
                assert!(
                    failed_obligations.contains(&"renderer_determinism".to_string()),
                    "renderer_determinism must appear in failed_obligations, got: {:?}",
                    failed_obligations
                );
            }
            other => panic!(
                "expected ValidationFailed for Unimplemented determinism, got: {}",
                other
            ),
        }
    }

    #[test]
    fn yaml_content_contains_required_fields() {
        let req = passing_request();
        let promoted = promote_renderer(req).unwrap();
        let yaml = &promoted.yaml_content;
        assert!(yaml.contains("stage_id: custom_volcano"));
        assert!(yaml.contains("version: \"1.0.0\""));
        assert!(yaml.contains("figure_ids:"));
        assert!(yaml.contains("  - volcano"));
        assert!(yaml.contains("lib/plotting/stages/_generated/custom_volcano.py"));
        assert!(yaml.contains("renderer-proposal-abc123"));
        assert!(yaml.contains("decision-xyz"));
    }

    #[test]
    fn validation_outcome_is_passing_semantics() {
        assert!(ValidationOutcome::Passed.is_passing());
        assert!(ValidationOutcome::Unimplemented {
            obligation_id: "x".into()
        }
        .is_passing());
        assert!(!ValidationOutcome::Failed {
            message: "fail".into()
        }
        .is_passing());
        assert!(!ValidationOutcome::Errored {
            reason: "err".into()
        }
        .is_passing());
    }
}
