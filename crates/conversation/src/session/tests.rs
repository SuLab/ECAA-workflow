//! Session state-machine tests. `use super::*;` pulls in the full
//! public session surface.

use super::*;
use ecaa_workflow_core::blocker::{BlockerContext, BlockerKind};

#[test]
fn greeting_to_intake_on_first_prose() {
    let mut s = Session::new(false);
    assert_eq!(s.state, SessionState::Greeting);
    s.try_transition(StateTrigger::AppendProse).unwrap();
    assert_eq!(s.state, SessionState::Intake);
}

#[test]
fn intake_to_followup_on_unresolved_discovery() {
    let mut s = Session::new(false);
    s.try_transition(StateTrigger::AppendProse).unwrap();
    s.try_transition(StateTrigger::DagBuiltWithUnresolvedDiscovery)
        .unwrap();
    assert_eq!(s.state, SessionState::IntakeFollowup);
}

#[test]
fn followup_back_to_intake_on_more_prose() {
    let mut s = Session::new(false);
    s.try_transition(StateTrigger::AppendProse).unwrap();
    s.try_transition(StateTrigger::DagBuiltWithUnresolvedDiscovery)
        .unwrap();
    s.try_transition(StateTrigger::AppendProse).unwrap();
    assert_eq!(s.state, SessionState::Intake);
}

#[test]
fn intake_to_pending_then_ready() {
    let mut s = Session::new(false);
    s.try_transition(StateTrigger::AppendProse).unwrap();
    s.try_transition(StateTrigger::ProposeSummaryConfirmation)
        .unwrap();
    assert_eq!(s.state, SessionState::PendingConfirmation { stage: None });
    s.try_transition(StateTrigger::UserClickedConfirm).unwrap();
    assert_eq!(s.state, SessionState::ReadyToEmit);
}

#[test]
fn pending_to_intake_on_reject_preserves_methods() {
    let mut s = Session::new(false);
    s.try_transition(StateTrigger::AppendProse).unwrap();
    s.intake_methods
        .set("preprocessing", Some("Cell Ranger".into()), None);
    s.try_transition(StateTrigger::ProposeSummaryConfirmation)
        .unwrap();
    s.try_transition(StateTrigger::UserClickedReject).unwrap();
    assert_eq!(s.state, SessionState::Intake);
    assert!(s.intake_methods.0.contains_key("preprocessing"));
}

#[test]
fn intake_skip_pending_to_ready() {
    let mut s = Session::new(false);
    s.try_transition(StateTrigger::AppendProse).unwrap();
    s.try_transition(StateTrigger::UserClickedConfirm).unwrap();
    assert_eq!(s.state, SessionState::ReadyToEmit);
}

#[test]
fn ready_to_emit_through_emitting() {
    let mut s = Session::new(false);
    s.try_transition(StateTrigger::AppendProse).unwrap();
    s.try_transition(StateTrigger::ProposeSummaryConfirmation)
        .unwrap();
    s.try_transition(StateTrigger::UserClickedConfirm).unwrap();
    s.try_transition(StateTrigger::EmitPackageStart).unwrap();
    assert_eq!(s.state, SessionState::Emitting);
    s.try_transition(StateTrigger::EmitPackageOk).unwrap();
    assert_eq!(s.state, SessionState::Emitted);
}

#[test]
fn emit_error_routes_to_blocked() {
    let mut s = Session::new(false);
    s.try_transition(StateTrigger::AppendProse).unwrap();
    s.try_transition(StateTrigger::ProposeSummaryConfirmation)
        .unwrap();
    s.try_transition(StateTrigger::UserClickedConfirm).unwrap();
    s.try_transition(StateTrigger::EmitPackageStart).unwrap();
    s.try_transition(StateTrigger::EmitPackageErr {
        reason: "disk full".into(),
    })
    .unwrap();
    match &s.state {
        SessionState::Blocked {
            reason,
            blocker_kind,
            ..
        } => {
            assert_eq!(reason, "disk full");
            assert!(matches!(blocker_kind, Some(BlockerKind::HostError { .. })));
        }
        other => panic!("expected Blocked, got {:?}", other),
    }
}

#[test]
fn harness_task_blocked_transitions_emitted_to_blocked_with_typed_kind() {
    // Transition wired so the server's /progress handler can surface
    // task-level blockers from the harness to the UI's BlockerCard
    // with the right recovery affordance.
    let mut s = Session::new(false);
    s.try_transition(StateTrigger::AppendProse).unwrap();
    s.try_transition(StateTrigger::UserClickedConfirm).unwrap();
    s.try_transition(StateTrigger::EmitPackageStart).unwrap();
    s.try_transition(StateTrigger::EmitPackageOk).unwrap();
    assert_eq!(s.state, SessionState::Emitted);

    s.try_transition(StateTrigger::HarnessTaskBlocked {
        task_id: "normalization".into(),
        detail: "mock synthetic".into(),
        blocker_kind: BlockerKind::DataShapeMismatch {
            expected: "matrix".into(),
            actual: "list".into(),
        },
    })
    .unwrap();

    match &s.state {
        SessionState::Blocked {
            reason,
            blocker_kind,
            context,
            ..
        } => {
            assert!(reason.contains("normalization"));
            assert!(matches!(
                blocker_kind,
                Some(BlockerKind::DataShapeMismatch { .. })
            ));
            let ctx = context.as_ref().unwrap();
            assert!(ctx.recovery_hints.as_deref().unwrap().contains("rerun"));
        }
        other => panic!("expected Blocked, got {:?}", other),
    }

    // OperatorUnblock resumes — transitions back to Intake per the
    // state machine (the SME continues the conversation).
    s.try_transition(StateTrigger::OperatorUnblock).unwrap();
    assert_eq!(s.state, SessionState::Intake);
}

#[test]
fn resolved_blocker_synthesizes_from_legacy_fields() {
    // A session deserialized from pre-PR-2.11.2 JSON has blocker_kind = None.
    // resolved_blocker() must fabricate BlockerKind::AgentError from reason.
    let state = SessionState::Blocked {
        blockers: vec![],
        reason: "stale synthetic blocker".into(),
        recovery_hint: "retry".into(),
        blocker_kind: None,
        context: None,
    };
    let (kind, ctx) = state.resolved_blocker().expect("Blocked has resolution");
    match kind {
        BlockerKind::AgentError { message } => {
            assert_eq!(message, "stale synthetic blocker");
        }
        other => panic!("expected AgentError fallback, got {:?}", other),
    }
    assert_eq!(ctx.recovery_hints.as_deref(), Some("retry"));
}

#[test]
fn resolved_blocker_prefers_structured_fields() {
    let state = SessionState::Blocked {
        blockers: vec![],
        reason: "legacy reason".into(),
        recovery_hint: "legacy hint".into(),
        blocker_kind: Some(BlockerKind::ValidationFailed {
            check: "schema".into(),
            message: "bad shape".into(),
            cause: None,
        }),
        context: Some(BlockerContext {
            timestamp: "2026-04-16T00:00:00Z".into(),
            recovery_hints: Some("rerun with fixed input".into()),
        }),
    };
    let (kind, ctx) = state.resolved_blocker().unwrap();
    assert!(matches!(kind, BlockerKind::ValidationFailed { .. }));
    assert_eq!(
        ctx.recovery_hints.as_deref(),
        Some("rerun with fixed input")
    );
}

#[test]
fn structured_capture_aws_sizing_fields_roundtrip() {
    // Canonical AWS-sizing keys: sample_count, coverage_depth,
    // cell_count, database_size_gb. The card shape must serialize so
    // existing StructuredCaptureTurnCard.tsx (which is generic over
    // StructuredCaptureField) consumes it unchanged.
    let card = StructuredCaptureTurnCard {
        title: "One more set of numbers before we size the run".into(),
        description: Some("Leave blank if unknown — we'll oversize.".into()),
        fields: vec![
            StructuredCaptureField {
                key: "sample_count".into(),
                label: "Number of biological samples".into(),
                placeholder: Some("e.g. 42".into()),
                required: false,
                multiline: false,
                kind: StructuredCaptureFieldKind::Integer,
            },
            StructuredCaptureField {
                key: "coverage_depth".into(),
                label: "Coverage depth (×)".into(),
                placeholder: Some("e.g. 30".into()),
                required: false,
                multiline: false,
                kind: StructuredCaptureFieldKind::Integer,
            },
            StructuredCaptureField {
                key: "cell_count".into(),
                label: "Cell count (single-cell)".into(),
                placeholder: Some("e.g. 5000".into()),
                required: false,
                multiline: false,
                kind: StructuredCaptureFieldKind::Integer,
            },
            StructuredCaptureField {
                key: "database_size_gb".into(),
                label: "Reference database size (GB)".into(),
                placeholder: Some("e.g. 150".into()),
                required: false,
                multiline: false,
                kind: StructuredCaptureFieldKind::Float,
            },
        ],
        initial_values: std::collections::BTreeMap::new(),
    };
    let json = serde_json::to_string(&card).unwrap();
    let back: StructuredCaptureTurnCard = serde_json::from_str(&json).unwrap();
    assert_eq!(card, back);
    // Verify the four canonical AWS keys round-trip.
    let keys: Vec<&str> = back.fields.iter().map(|f| f.key.as_str()).collect();
    assert_eq!(
        keys,
        vec![
            "sample_count",
            "coverage_depth",
            "cell_count",
            "database_size_gb"
        ]
    );
    // Verify kind is serialized in snake_case so ts-rs bindings match.
    assert!(
        json.contains("\"kind\":\"integer\""),
        "kind should snake_case serialize: {json}"
    );
    assert!(
        json.contains("\"kind\":\"float\""),
        "float kind should serialize: {json}"
    );
}

#[test]
fn structured_capture_field_kind_default_is_string() {
    let json = r#"{"key":"foo","label":"Foo"}"#;
    let field: StructuredCaptureField = serde_json::from_str(json).unwrap();
    assert_eq!(field.kind, StructuredCaptureFieldKind::String);
    assert!(!field.required);
    assert!(!field.multiline);
}

#[test]
fn amend_start_from_emitted_transitions_to_amending() {
    let mut s = Session::new(false);
    // Drive to Emitted via the normal flow.
    s.try_transition(StateTrigger::AppendProse).unwrap();
    s.try_transition(StateTrigger::ProposeSummaryConfirmation)
        .unwrap();
    s.try_transition(StateTrigger::UserClickedConfirm).unwrap();
    s.try_transition(StateTrigger::EmitPackageStart).unwrap();
    s.try_transition(StateTrigger::EmitPackageOk).unwrap();
    assert_eq!(s.state, SessionState::Emitted);

    s.try_transition(StateTrigger::AmendStart {
        target_stage: "differential_expression".into(),
        invalidated_tasks: vec![
            "differential_expression".into(),
            "validate_differential_expression".into(),
        ],
    })
    .unwrap();
    match &s.state {
        SessionState::Amending {
            target_stage,
            invalidated_tasks,
        } => {
            assert_eq!(target_stage, "differential_expression");
            assert_eq!(invalidated_tasks.len(), 2);
        }
        other => panic!("expected Amending, got {:?}", other),
    }
}

#[test]
fn amend_ready_from_amending_transitions_to_ready_to_emit() {
    let mut s = Session::new(false);
    s.state = SessionState::Amending {
        target_stage: "any".into(),
        invalidated_tasks: vec![],
    };
    s.try_transition(StateTrigger::AmendReady).unwrap();
    assert_eq!(s.state, SessionState::ReadyToEmit);
}

#[test]
fn amend_start_from_non_emitted_is_rejected() {
    let mut s = Session::new(false);
    s.try_transition(StateTrigger::AppendProse).unwrap();
    // Intake, not Emitted — AmendStart must be illegal.
    let err = s
        .try_transition(StateTrigger::AmendStart {
            target_stage: "x".into(),
            invalidated_tasks: vec![],
        })
        .unwrap_err();
    assert!(err.to_string().contains("AmendStart"));
}

#[test]
fn branch_from_inherits_intake_state_and_resets_run_state() {
    let mut parent = Session::new(false);
    parent.try_transition(StateTrigger::AppendProse).unwrap();
    parent.intake_prose = "single cell scRNA-seq from human IVD".into();
    // Mints a confirmation token to mirror the old
    // `user_confirmed = true` with
    // mint_confirmation_token (after seeding a pending_emission_id).
    parent.pending_emission_id = Some(uuid::Uuid::new_v4());
    let _ = parent.mint_confirmation_token(
        chrono::Utc::now(),
        crate::audit_actor::AuditActor::User("test".into()),
    );
    parent.emitted_package_path = Some(std::path::PathBuf::from("/tmp/parent-package"));
    {
        let conv = std::sync::Arc::make_mut(&mut parent.conversation);
        conv.push(Turn::user("hello"));
        conv.push(Turn::assistant("hi back"));
    }

    let child = Session::branch_from(&parent, false);

    // Inherited fields.
    assert_eq!(child.intake_prose, parent.intake_prose);
    assert_eq!(child.conversation.len(), parent.conversation.len());
    assert_eq!(child.state, parent.state);

    // Reset fields — branched run starts fresh. Confirmation token is
    // None and the pending emission id is also cleared.
    assert!(child.confirmation_token.is_none());
    assert!(child.pending_emission_id.is_none());
    assert!(child.emitted_package_path.is_none());
    assert!(child.harness_events.is_empty());
    assert!(child.tool_call_log.is_empty());

    // Lineage points back at the parent.
    let lineage = child.lineage.expect("branch must record lineage");
    assert_eq!(lineage.parent_session_id, parent.id);
    assert_eq!(lineage.branched_from_turn_index, Some(2));
    assert_ne!(child.id, parent.id, "branch must allocate a new id");
}

#[test]
fn branch_from_task_inherits_authoritative_workflow_dag_and_resets_target() {
    use ecaa_workflow_core::dag::TaskState;
    use ecaa_workflow_core::workflow_contracts::task_node::{TaskNode, WorkflowDag};

    let mut parent = Session::new(false);
    parent.state = SessionState::Emitted;
    parent.workflow_dag = Some(WorkflowDag {
        id: "wf-branch-regression".into(),
        nodes: vec![TaskNode::skeleton(
            "data_acquisition",
            "Acquire tractable test dataset",
        )],
        edges: vec![],
        assumptions: Default::default(),
        source_template: None,
    });
    parent.task_states.insert(
        "data_acquisition".into(),
        TaskState::Completed {
            result: serde_json::json!({"ok": true}),
        },
    );

    let child = Session::branch_from_at_task(&parent, false, Some("data_acquisition".into()));

    assert!(
        child.workflow_dag.is_some(),
        "branch must retain the authoritative workflow_dag for UI DAG rendering and re-emission"
    );
    let child_dag = child
        .current_dag()
        .expect("branch child must expose a current DAG");
    let task = child_dag
        .tasks
        .get("data_acquisition")
        .expect("branched DAG must keep the branch target task");
    assert!(
        matches!(task.state, TaskState::Ready),
        "branch target should reset from completed to ready"
    );
}

#[test]
fn branch_from_root_session_records_no_turn_index() {
    let parent = Session::new(false);
    let child = Session::branch_from(&parent, false);
    let lineage = child.lineage.expect("branch must record lineage");
    assert!(lineage.branched_from_turn_index.is_none());
}

#[test]
fn root_session_has_no_lineage() {
    let s = Session::new(false);
    assert!(s.lineage.is_none());
}

#[test]
fn resolved_blocker_returns_none_for_non_blocked() {
    assert!(SessionState::Greeting.resolved_blocker().is_none());
    assert!(SessionState::ReadyToEmit.resolved_blocker().is_none());
}

#[test]
fn blocked_unblocks_to_intake() {
    let mut s = Session::new(false);
    s.try_transition(StateTrigger::AppendProse).unwrap();
    s.try_transition(StateTrigger::InfraError {
        reason: "api down".into(),
    })
    .unwrap();
    s.try_transition(StateTrigger::OperatorUnblock).unwrap();
    assert_eq!(s.state, SessionState::Intake);
}

#[test]
fn emitted_rejects_back_to_intake() {
    let mut s = Session::new(false);
    s.try_transition(StateTrigger::AppendProse).unwrap();
    s.try_transition(StateTrigger::ProposeSummaryConfirmation)
        .unwrap();
    s.try_transition(StateTrigger::UserClickedConfirm).unwrap();
    s.try_transition(StateTrigger::EmitPackageStart).unwrap();
    s.try_transition(StateTrigger::EmitPackageOk).unwrap();
    // Emitted absorbs subsequent triggers, never goes back to Intake.
    s.try_transition(StateTrigger::AppendProse).unwrap();
    assert_eq!(s.state, SessionState::Emitted);
}

#[test]
fn infra_error_from_emitted_lands_in_blocked() {
    // Regression guard for arm order in the state machine table. The
    // (_, InfraError { reason }) → Blocked wildcard must appear before
    // the (Emitted, _) catch-all so an InfraError fired from Emitted
    // transitions to Blocked rather than being swallowed.
    let mut s = Session::new(false);
    s.try_transition(StateTrigger::AppendProse).unwrap();
    s.try_transition(StateTrigger::ProposeSummaryConfirmation)
        .unwrap();
    s.try_transition(StateTrigger::UserClickedConfirm).unwrap();
    s.try_transition(StateTrigger::EmitPackageStart).unwrap();
    s.try_transition(StateTrigger::EmitPackageOk).unwrap();
    assert_eq!(s.state, SessionState::Emitted);
    // Now the host fires an infra error from Emitted — must reach
    // Blocked, NOT stay in Emitted.
    s.try_transition(StateTrigger::InfraError {
        reason: "anthropic API unreachable mid-execution".into(),
    })
    .unwrap();
    match &s.state {
        SessionState::Blocked { reason, .. } => {
            assert!(reason.contains("anthropic API unreachable"));
        }
        other => panic!("expected Blocked from Emitted+InfraError, got {:?}", other),
    }
    // And the Blocked → Intake recovery path still works after this
    // round trip when `emitted_package_path` is None (unit-test
    // scenario — in production the emit service populates the path
    // alongside EmitPackageOk; see
    // `blocked_unblocks_to_emitted_when_package_emitted` for the
    // post-emit variant that exercises the path-aware branch).
    s.try_transition(StateTrigger::OperatorUnblock).unwrap();
    assert_eq!(s.state, SessionState::Intake);
}

#[test]
fn blocked_unblocks_to_emitted_when_package_emitted() {
    // Regression guard for the IVD live e2e multi-blocker bug: when
    // the session has an emitted package, Blocked → OperatorUnblock
    // must restore Emitted (not Intake) so subsequent harness
    // task_blocked events can transition the session to Blocked
    // again via `service::block_from_harness`'s Emitted-only guard.
    // Before this fix, the first blocker cycle worked but every
    // subsequent blocker was silently dropped by the service-layer
    // guard because the session was sitting in Intake.
    use ecaa_workflow_core::blocker::BlockerKind;
    let mut s = Session::new(false);
    s.try_transition(StateTrigger::AppendProse).unwrap();
    s.try_transition(StateTrigger::ProposeSummaryConfirmation)
        .unwrap();
    s.try_transition(StateTrigger::UserClickedConfirm).unwrap();
    s.try_transition(StateTrigger::EmitPackageStart).unwrap();
    s.try_transition(StateTrigger::EmitPackageOk).unwrap();
    s.emitted_package_path = Some(std::path::PathBuf::from("/tmp/pkg"));
    assert_eq!(s.state, SessionState::Emitted);

    // First harness blocker cycle — runs end-to-end.
    s.try_transition(StateTrigger::HarnessTaskBlocked {
        task_id: "discover_preprocessing".into(),
        detail: "Awaiting SME approval".into(),
        blocker_kind: BlockerKind::AwaitingSmeSelection {
            stage_id: "discover_preprocessing".into(),
            candidates: vec!["cellranger".into(), "starsolo".into()],
        },
    })
    .unwrap();
    assert!(matches!(s.state, SessionState::Blocked { .. }));
    s.try_transition(StateTrigger::OperatorUnblock).unwrap();
    assert_eq!(
        s.state,
        SessionState::Emitted,
        "post-emit unblock must restore Emitted, not Intake"
    );

    // Second harness blocker cycle — the real regression test.
    // Before the fix, this transition would have been silently
    // dropped because the session would have been in Intake.
    s.try_transition(StateTrigger::HarnessTaskBlocked {
        task_id: "discover_normalization".into(),
        detail: "Awaiting SME approval".into(),
        blocker_kind: BlockerKind::AwaitingSmeSelection {
            stage_id: "discover_normalization".into(),
            candidates: vec!["sctransform".into(), "lognormalize".into()],
        },
    })
    .unwrap();
    assert!(
        matches!(s.state, SessionState::Blocked { .. }),
        "second harness blocker must transition Emitted → Blocked"
    );
    s.try_transition(StateTrigger::OperatorUnblock).unwrap();
    assert_eq!(s.state, SessionState::Emitted);
}

#[test]
fn emitted_still_absorbs_non_infra_triggers() {
    // The Emitted state must absorb non-infra triggers. Verify
    // AppendProse, DagBuiltWithUnresolvedDiscovery,
    // ProposeSummaryConfirmation, UserClickedConfirm/Reject all still
    // bounce off Emitted.
    let mut s = Session::new(false);
    s.try_transition(StateTrigger::AppendProse).unwrap();
    s.try_transition(StateTrigger::ProposeSummaryConfirmation)
        .unwrap();
    s.try_transition(StateTrigger::UserClickedConfirm).unwrap();
    s.try_transition(StateTrigger::EmitPackageStart).unwrap();
    s.try_transition(StateTrigger::EmitPackageOk).unwrap();
    for trigger in [
        StateTrigger::AppendProse,
        StateTrigger::DagBuiltWithUnresolvedDiscovery,
        StateTrigger::ProposeSummaryConfirmation,
        StateTrigger::UserClickedConfirm,
        StateTrigger::UserClickedReject,
    ] {
        s.try_transition(trigger).unwrap();
        assert_eq!(s.state, SessionState::Emitted);
    }
}

#[test]
fn ready_to_emit_rejects_intake_trigger() {
    let mut s = Session::new(false);
    s.try_transition(StateTrigger::AppendProse).unwrap();
    s.try_transition(StateTrigger::ProposeSummaryConfirmation)
        .unwrap();
    s.try_transition(StateTrigger::UserClickedConfirm).unwrap();
    // ReadyToEmit cannot accept further AppendProse — must emit or block.
    let err = s.try_transition(StateTrigger::AppendProse);
    assert!(err.is_err());
}

#[test]
fn intake_methods_serde_roundtrip_to_core() {
    let mut m = IntakeMethodsSerde::default();
    m.set("preprocessing", Some("Cell Ranger 7".into()), None);
    m.set(
        "batch_correction",
        Some("scVI".into()),
        Some(("batch_correction_required".into(), serde_json::json!(true))),
    );
    let core = m.to_core();
    assert_eq!(core.get("preprocessing").unwrap().method, "Cell Ranger 7");
    let bc = core.get("batch_correction").unwrap();
    assert_eq!(bc.method, "scVI");
    assert_eq!(
        bc.fields.get("batch_correction_required").unwrap(),
        &serde_json::json!(true)
    );
}

/// `archetype_snapshot` defaults to `None` on fresh
/// sessions and round-trips through serde. Branches inherit the
/// parent's snapshot so amendments stay on the same archetype
/// version.
#[test]
fn archetype_snapshot_defaults_to_none_and_inherits_through_branch() {
    use ecaa_workflow_core::archetype::ArchetypeDefinition;
    use std::collections::BTreeMap;

    let mut parent = Session::new(false);
    assert!(
        parent.archetype_snapshot.is_none(),
        "fresh session should have no archetype snapshot"
    );

    // Pin a snapshot on the parent (composer would do this at first
    // archetype-match emit).
    let snapshot = ArchetypeDefinition {
        schema_version: ecaa_workflow_core::archetype::CURRENT_ARCHETYPE_SCHEMA_VERSION.into(),
        id: "single_cell_de".into(),
        version: "1.0.0".into(),
        description: "Test archetype".into(),
        sme_summary: "Pinned snapshot".into(),
        goal_data: "data:3917".into(),
        goal_format: Some("format:3590".into()),
        atoms: vec![],
        slot_mappings: BTreeMap::new(),
        compose: vec![],
        slots: None,
        cross_dependencies: vec![],
        claim_boundary: None,
        project_class: "bioinformatics".into(),
        modality_hint: None,
        goal_kind_hint: None,
        preferred_container: None,
        runtime_baseline: Default::default(),
        cross_omics_modalities: vec![],
    };
    parent.archetype_snapshot = Some(snapshot.clone());

    // Branch inherits the snapshot.
    let child = Session::branch_from(&parent, false);
    assert_eq!(
        child.archetype_snapshot.as_ref(),
        Some(&snapshot),
        "branched session must inherit parent's archetype snapshot"
    );

    // Serde round-trip preserves the snapshot.
    let json = serde_json::to_string(&parent).unwrap();
    let back: Session = serde_json::from_str(&json).unwrap();
    assert_eq!(back.archetype_snapshot.as_ref(), Some(&snapshot));
}

/// Pre-S6.9 session JSON (no `archetype_snapshot`
/// field) deserializes with `None` via `#[serde(default)]`.
#[test]
fn archetype_snapshot_back_compat_loads_legacy_session_json() {
    let legacy_json = serde_json::json!({
        "id": "00000000-0000-4000-8000-000000000000",
        "created_at": "2026-04-24T00:00:00Z",
        "last_activity": "2026-04-24T00:00:00Z",
        "state": {"kind": "greeting"},
        "conversation": [],
        "intake_prose": "",
        "intake_methods": {},
        // The legacy `user_confirmed: bool` wire shape must continue
        // to deserialize. The custom serde adapter
        // `deserialize_confirmation_token_legacy` reads this field
        // name and folds the bool through to `None`.
        "user_confirmed": false,
    });
    let session: Session = serde_json::from_value(legacy_json).unwrap();
    assert!(
        session.archetype_snapshot.is_none(),
        "legacy session JSON must deserialize with archetype_snapshot = None"
    );
    // Even legacy `user_confirmed: true` deserializes to None
    // (fail-safe re-confirm). We don't assert the exact value here
    // because the legacy_json above uses false; the dedicated
    // back-compat test below covers the true case.
    assert!(
        session.confirmation_token.is_none(),
        "legacy `user_confirmed: false` must deserialize to no token"
    );
}

/// Sessions persisted before the ConfirmationToken migration carry
/// `user_confirmed: true`. The legacy-serde adapter must fold this
/// through to `confirmation_token: None` (fail-safe re-confirm). A bound
/// token cannot be retroactively fabricated because no emission_id +
/// summary_hash existed on the wire; the SME must re-confirm on next
/// interaction.
#[test]
fn c2_legacy_user_confirmed_true_loads_as_no_token() {
    let legacy_json = serde_json::json!({
        "id": "00000000-0000-4000-8000-000000000000",
        "created_at": "2026-04-24T00:00:00Z",
        "last_activity": "2026-04-24T00:00:00Z",
        "state": {"kind": "greeting"},
        "conversation": [],
        "intake_prose": "",
        "intake_methods": {},
        "user_confirmed": true,
    });
    let session: Session = serde_json::from_value(legacy_json).unwrap();
    assert!(
        session.confirmation_token.is_none(),
        "legacy `user_confirmed: true` must deserialize to None — \
         no emission_id + summary_hash retroactively bindable"
    );
    assert!(
        !session.is_confirmed(),
        "C2 fail-safe: legacy `user_confirmed: true` must require re-confirm"
    );
}

/// D9: an already-emitted session whose persisted form is the
/// legacy `user_confirmed: true` shape must NOT prompt the SME to
/// re-confirm after a server restart. The durable RO-Crate on disk
/// IS the artifact of a prior confirmation, so `is_confirmed()`
/// short-circuits to `true` whenever `state == Emitted` and
/// `emitted_package_path` is set — even though the legacy serde
/// adapter folded the bool through to `confirmation_token: None`.
#[test]
fn d9_emitted_session_with_legacy_user_confirmed_still_reports_confirmed() {
    let legacy_json = serde_json::json!({
        "id": "00000000-0000-4000-8000-000000000000",
        "created_at": "2026-04-24T00:00:00Z",
        "last_activity": "2026-04-24T00:00:00Z",
        "state": {"kind": "emitted"},
        "conversation": [],
        "intake_prose": "scRNA-seq IVD",
        "intake_methods": {},
        "user_confirmed": true,
        "emitted_package_path": "/tmp/scripps-pkg-deadbeef",
    });
    let session: Session = serde_json::from_value(legacy_json).unwrap();
    assert!(
        session.confirmation_token.is_none(),
        "legacy adapter still folds bool → None on the in-memory shape"
    );
    assert!(
        session.pending_emission_id.is_none(),
        "no pending emission on a terminal Emitted session"
    );
    assert!(
        session.is_confirmed(),
        "D9: Emitted session with durable package_path must report \
         is_confirmed()=true so the LLM does not prompt re-confirmation"
    );
}

/// D9 sibling: an Emitted session persisted under the *new*
/// (post-C2) shape — with the per-emit token already consumed by a
/// prior successful emit — must also report `is_confirmed()=true`.
/// Without the Emitted short-circuit, `ConfirmationToken::authorizes`
/// returns false on a consumed token and the LLM would see
/// `user_confirmed=false` after every server restart even for brand-new
/// sessions, not just legacy migrated ones.
#[test]
fn d9_emitted_session_with_consumed_token_still_reports_confirmed() {
    let mut s = Session::new(false);
    s.try_transition(StateTrigger::AppendProse).unwrap();
    s.try_transition(StateTrigger::ProposeSummaryConfirmation)
        .unwrap();
    s.pending_emission_id = Some(uuid::Uuid::new_v4());
    let _ = s.mint_confirmation_token(
        chrono::Utc::now(),
        crate::audit_actor::AuditActor::User("sme".into()),
    );
    s.try_transition(StateTrigger::UserClickedConfirm).unwrap();
    s.try_transition(StateTrigger::EmitPackageStart).unwrap();
    s.try_transition(StateTrigger::EmitPackageOk).unwrap();
    // simulate the emit_package_post_ok handler's single-use latch
    // consume + the emitted_package_path that emit::emit_with_conversation_log
    // would have written.
    if let Some(t) = s.confirmation_token.as_mut() {
        t.consume();
    }
    s.emitted_package_path = Some(std::path::PathBuf::from("/tmp/scripps-pkg-cafef00d"));
    assert_eq!(s.state, SessionState::Emitted);
    assert!(
        s.confirmation_token
            .as_ref()
            .is_some_and(|t| t.is_consumed()),
        "post-emit token must be consumed by emit_package_post_ok"
    );
    assert!(
        s.is_confirmed(),
        "D9: Emitted session with a consumed token but a durable package \
         must report is_confirmed()=true so a fresh server process does \
         not prompt the SME to re-confirm an already-emitted package"
    );
}

/// D9 guard: `is_confirmed()` MUST NOT short-circuit for non-Emitted
/// terminal states or for Emitted states that lack a durable package
/// path. Defense against an over-broad relaxation that would let the
/// LLM call emit_package from ReadyToEmit without a fresh confirm.
#[test]
fn d9_emitted_short_circuit_requires_both_state_and_package_path() {
    // Emitted but no emitted_package_path → fall through to the
    // token+pending check, which returns false on a fresh session.
    let mut s = Session::new(false);
    s.state = SessionState::Emitted;
    assert!(s.emitted_package_path.is_none());
    assert!(
        !s.is_confirmed(),
        "Emitted state without a package path must NOT report confirmed"
    );

    // ReadyToEmit with an emitted_package_path (e.g. stale field from
    // a prior emit cycle followed by amend) → still requires a fresh
    // token+pending pair.
    let mut s2 = Session::new(false);
    s2.state = SessionState::ReadyToEmit;
    s2.emitted_package_path = Some(std::path::PathBuf::from("/tmp/stale"));
    assert!(
        !s2.is_confirmed(),
        "ReadyToEmit must still require a fresh confirmation token \
         even if a stale emitted_package_path is present"
    );
}
