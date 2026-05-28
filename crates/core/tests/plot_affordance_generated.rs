//! Test scaffolding for generated-renderer promotion.
//!
//! Tests are authored here but execution is deferred until the harness
//! sandbox lands. Any test that depends on
//! running Python is `#[ignore]`; pure-Rust serde + gate tests run
//! immediately.

use scripps_workflow_core::plot_affordance::promotion::{
    promote_renderer, PromotionError, RendererPromotionRequest, ValidationOutcome, ValidationRow,
};
use scripps_workflow_core::plot_affordance::sandbox::{check_drafted_renderer, SandboxOutcome};
use scripps_workflow_core::sandbox_policy::{SandboxPolicy, SandboxRefusal};

// ---------------------------------------------------------------------------
// DraftedRenderer serde round-trip
// ---------------------------------------------------------------------------

/// The `DraftedRenderer` type lives in `crates/conversation` which
/// `crates/core` doesn't depend on. This test exercises the equivalent
/// plain-struct round-trip using `ValidationRow` + `ValidationOutcome`
/// (the mirror types used by the promotion gate in `crates/core`).
#[test]
fn validation_row_round_trips_serde() {
    let row = ValidationRow {
        obligation_id: "renderer_dpi_300".into(),
        outcome: ValidationOutcome::Passed,
    };
    let json = serde_json::to_string(&row).unwrap();
    let back: ValidationRow = serde_json::from_str(&json).unwrap();
    assert_eq!(row, back);
}

#[test]
fn validation_outcome_failed_round_trips_serde() {
    let outcome = ValidationOutcome::Failed {
        message: "PNG DPI was 72, expected 300".into(),
    };
    let json = serde_json::to_string(&outcome).unwrap();
    let back: ValidationOutcome = serde_json::from_str(&json).unwrap();
    assert_eq!(outcome, back);
}

#[test]
fn validation_outcome_errored_round_trips_serde() {
    let outcome = ValidationOutcome::Errored {
        reason: "quality_scorer.py not found".into(),
    };
    let json = serde_json::to_string(&outcome).unwrap();
    let back: ValidationOutcome = serde_json::from_str(&json).unwrap();
    assert_eq!(outcome, back);
}

// ---------------------------------------------------------------------------
// SandboxOutcome variant tag
// ---------------------------------------------------------------------------

#[test]
fn sandbox_outcome_refused_carries_refusals() {
    let outcome = SandboxOutcome::Refused {
        refusals: vec![SandboxRefusal::StaticAnalysisRequired],
    };
    match &outcome {
        SandboxOutcome::Refused { refusals } => {
            assert_eq!(refusals.len(), 1);
            assert!(refusals.contains(&SandboxRefusal::StaticAnalysisRequired));
        }
        SandboxOutcome::StaticChecksPassed { .. } => panic!("wrong variant"),
    }
}

#[test]
fn sandbox_outcome_static_checks_passed_carries_task_node_id() {
    let outcome = SandboxOutcome::StaticChecksPassed {
        task_node_id: "generated_renderer_volcano".into(),
    };
    match &outcome {
        SandboxOutcome::StaticChecksPassed { task_node_id } => {
            assert_eq!(task_node_id, "generated_renderer_volcano");
        }
        SandboxOutcome::Refused { .. } => panic!("wrong variant"),
    }
}

// ---------------------------------------------------------------------------
// promote_renderer rejects when validation_report has any failed obligation
// ---------------------------------------------------------------------------

#[test]
fn promote_renderer_rejects_any_failed_obligation() {
    let req = RendererPromotionRequest {
        proposal_id: "renderer-proposal-test".into(),
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
                outcome: ValidationOutcome::Failed {
                    message: "PNG DPI is 72, expected 300".into(),
                },
            },
            ValidationRow {
                obligation_id: "renderer_theme_parity".into(),
                outcome: ValidationOutcome::Passed,
            },
            ValidationRow {
                obligation_id: "renderer_determinism".into(),
                outcome: ValidationOutcome::Unimplemented {
                    obligation_id: "renderer_determinism".into(),
                },
            },
        ],
        sme_approval_decision_id: Some("decision-xyz".into()),
        target_stage_id: "custom_volcano".into(),
        version: "1.0.0".into(),
        figure_ids: vec!["volcano".into()],
        renderer_module: "lib/plotting/stages/_generated/custom_volcano.py".into(),
        dry_run: true,
        output_dir: String::new(),
    };

    let err = promote_renderer(req).unwrap_err();
    match err {
        PromotionError::ValidationFailed { failed_obligations } => {
            assert!(
                failed_obligations.contains(&"renderer_dpi_300".to_string()),
                "expected renderer_dpi_300 in failed_obligations: {:?}",
                failed_obligations
            );
        }
        other => panic!("expected ValidationFailed, got: {}", other),
    }
}

#[test]
fn promote_renderer_rejects_on_sandbox_refused() {
    let req = RendererPromotionRequest {
        proposal_id: "renderer-proposal-test".into(),
        drafted_module_source: "import numpy as np\n".into(),
        sandbox_outcome: SandboxOutcome::Refused {
            refusals: vec![SandboxRefusal::StaticAnalysisRequired],
        },
        validation_report: vec![ValidationRow {
            obligation_id: "renderer_dpi_300".into(),
            outcome: ValidationOutcome::Passed,
        }],
        sme_approval_decision_id: Some("decision-xyz".into()),
        target_stage_id: "custom_volcano".into(),
        version: "1.0.0".into(),
        figure_ids: vec!["volcano".into()],
        renderer_module: "lib/plotting/stages/_generated/custom_volcano.py".into(),
        dry_run: true,
        output_dir: String::new(),
    };

    let err = promote_renderer(req).unwrap_err();
    assert!(
        matches!(err, PromotionError::SandboxRefused { .. }),
        "expected SandboxRefused, got: {}",
        err
    );
}

#[test]
fn promote_renderer_rejects_missing_sme_approval() {
    let req = RendererPromotionRequest {
        proposal_id: "renderer-proposal-test".into(),
        drafted_module_source: "import numpy as np\n".into(),
        sandbox_outcome: SandboxOutcome::StaticChecksPassed {
            task_node_id: "generated_renderer_volcano".into(),
        },
        validation_report: vec![],
        sme_approval_decision_id: None,
        target_stage_id: "custom_volcano".into(),
        version: "1.0.0".into(),
        figure_ids: vec!["volcano".into()],
        renderer_module: "lib/plotting/stages/_generated/custom_volcano.py".into(),
        dry_run: true,
        output_dir: String::new(),
    };

    let err = promote_renderer(req).unwrap_err();
    assert!(
        matches!(err, PromotionError::MissingSmeApproval),
        "expected MissingSmeApproval, got: {}",
        err
    );
}

#[test]
fn promote_renderer_succeeds_dry_run_all_gates_pass() {
    let req = RendererPromotionRequest {
        proposal_id: "renderer-proposal-test".into(),
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
            // Phase C8: renderer_determinism is now fully implemented via
            // DeterminismRunner. Must be Passed (not Unimplemented) to proceed.
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
    };

    let promoted = promote_renderer(req).unwrap();
    assert!(promoted.was_dry_run);
    assert!(promoted.yaml_content.contains("custom_volcano"));
}

// ---------------------------------------------------------------------------
// Static check entry-point integration
// ---------------------------------------------------------------------------

#[test]
fn check_drafted_renderer_passes_with_permissive_policy() {
    let mut policy = SandboxPolicy::default_strict();
    policy.require_static_analysis = false;
    policy.require_human_review_for_high_risk = false;

    let outcome = check_drafted_renderer(
        "import numpy as np\n",
        &["volcano".to_string()],
        "custom_volcano",
        &policy,
    );
    assert!(
        matches!(outcome, SandboxOutcome::StaticChecksPassed { .. }),
        "expected StaticChecksPassed, got refused"
    );
}

#[test]
fn check_drafted_renderer_refused_under_strict_policy() {
    let policy = SandboxPolicy::default_strict();
    let outcome = check_drafted_renderer(
        "import numpy as np\n",
        &["volcano".to_string()],
        "custom_volcano",
        &policy,
    );
    assert!(
        matches!(outcome, SandboxOutcome::Refused { .. }),
        "expected Refused under default_strict, got passed"
    );
}
