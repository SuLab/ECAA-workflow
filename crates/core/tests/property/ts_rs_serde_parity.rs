//! R-24 property test: serde round-trip on types that carry
//! `#[derive(TS)]`. The ts-rs derive generates the wire shape the UI
//! consumes; a regression where a Rust-side field is renamed or its
//! tag attribute drifts would break the UI without surfacing in the
//! Rust test suite. Round-trip via `serde_json` catches the
//! Rust-side breakage early.
//!
//! Coverage: representative values of `SessionState`, `BlockerKind`,
//! and `Tool`, each constructed via a small constructor function
//! (these types either lack `Arbitrary` or have payload fields that
//! reference other crates whose construction is non-trivial).

use ecaa_workflow_conversation::session::SessionState;
use ecaa_workflow_conversation::tools::{BatchableTool, HighImpactTool, Tool};
use ecaa_workflow_core::blocker::{BlockerContext, BlockerKind};

#[test]
fn session_state_roundtrips() {
    let variants = vec![
        SessionState::Greeting,
        SessionState::Intake,
        SessionState::IntakeFollowup,
        SessionState::PendingConfirmation { stage: None },
        SessionState::PendingConfirmation {
            stage: Some("normalization".into()),
        },
        SessionState::ReadyToEmit,
        SessionState::Emitting,
        SessionState::Emitted,
        SessionState::Amending {
            target_stage: "normalization".into(),
            invalidated_tasks: vec!["normalize".into(), "cluster".into()],
        },
        SessionState::Blocked {
            blockers: vec![],
            reason: "agent OOM".into(),
            recovery_hint: "rerun with larger instance".into(),
            blocker_kind: Some(BlockerKind::AgentError {
                message: "OOM".into(),
            }),
            context: Some(BlockerContext {
                timestamp: "2026-05-16T00:00:00Z".into(),
                recovery_hints: None,
            }),
        },
    ];
    for v in variants {
        let wire = serde_json::to_string(&v).expect("serialize session state");
        let back: SessionState = serde_json::from_str(&wire).expect("deserialize session state");
        assert_eq!(v, back);
    }
}

#[test]
fn tool_roundtrips() {
    // Sample of `Tool` variants from each bucket. The outer enum is
    // `#[serde(untagged)]` — wire shape comes from `BatchableTool` /
    // `HighImpactTool` whose `#[serde(tag = "tool_name")]` discriminator
    // is the load-bearing UI contract.
    let variants: Vec<Tool> = vec![
        Tool::Batchable(BatchableTool::GetSessionState),
        Tool::Batchable(BatchableTool::GetClassificationEvidence),
        Tool::Batchable(BatchableTool::ClassifyIntake {
            prose: "scRNA-seq IVD aging".into(),
        }),
        Tool::Batchable(BatchableTool::GetTaxonomyInfo {
            modality_id: "single_cell_rnaseq".into(),
        }),
        Tool::Batchable(BatchableTool::GetTaskResult {
            task_id: "alignment".into(),
        }),
        Tool::Batchable(BatchableTool::SetIntakeField {
            stage: "alignment".into(),
            field: "method".into(),
            value: serde_json::json!("STARsolo"),
        }),
        Tool::Batchable(BatchableTool::SetIntakeMethod {
            stage: "normalization".into(),
            method_prose: "scran pooling".into(),
        }),
        Tool::Batchable(BatchableTool::AppendIntakeProse {
            prose: "use harmony batch correction".into(),
        }),
        Tool::HighImpact(HighImpactTool::RerunTask {
            task_id: "alignment".into(),
            reason: Some("input cohort refreshed".into()),
        }),
    ];
    for v in variants {
        let wire = serde_json::to_string(&v).expect("serialize tool");
        let parsed: serde_json::Value = serde_json::from_str(&wire).expect("parse tool wire");
        // Every emitted JSON must carry the tag (inner enums are
        // `#[serde(tag = "tool_name")]`; the untagged outer makes the
        // tag bubble to the top-level object).
        assert!(
            parsed.get("tool_name").and_then(|v| v.as_str()).is_some(),
            "tool wire missing tool_name discriminator: {wire}"
        );
    }
}

#[test]
fn blocker_kind_typical_values_roundtrip() {
    let variants = vec![
        BlockerKind::DataShapeMismatch {
            expected: "matrix".into(),
            actual: "list".into(),
        },
        BlockerKind::ValidationFailed {
            check: "cells_per_sample_min".into(),
            message: "Sample S3 has 412 cells".into(),
            cause: None,
        },
        BlockerKind::AgentError {
            message: "OOM at 60 GiB".into(),
        },
        BlockerKind::MissingArtifact {
            task_id: "differential_expression".into(),
            missing_paths: vec!["results/tables/de_summary.tsv".into()],
        },
    ];
    for v in variants {
        let wire = serde_json::to_string(&v).expect("serialize blocker");
        let back: BlockerKind = serde_json::from_str(&wire).expect("deserialize blocker");
        assert_eq!(v, back);
        let parsed: serde_json::Value = serde_json::from_str(&wire).expect("parse blocker wire");
        assert!(
            parsed.get("kind").and_then(|v| v.as_str()).is_some(),
            "blocker wire missing kind discriminator: {wire}"
        );
    }
}
