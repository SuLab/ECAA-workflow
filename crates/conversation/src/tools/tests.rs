//! Tools tests. `use super::*;` pulls in the full public tools surface.

use super::*;
use crate::session::SessionState;
// Bring the bucket-enum variants directly into scope so the tests can
// continue to write `Tool::Batchable(BatchableTool::X { .. })` etc. via
// the shorter `bt!` / `hi!` macros below. The bucket types themselves
// are also re-exported by `super::*` above (they live in `tools/mod.rs`).
#[allow(unused_imports)]
use super::{BatchableTool, HighImpactTool};

/// Shorthand for constructing a `Tool::Batchable(BatchableTool::...)`
/// from a variant body. Lets the existing fixture closures stay close
/// to their pre-R-2 shape (`bt!(ClassifyIntake { prose: "x".into() })`
/// vs. `Tool::Batchable(BatchableTool::ClassifyIntake { prose: ... })`).
macro_rules! bt {
    ($variant:ident $({ $($body:tt)* })?) => {
        Tool::Batchable(BatchableTool::$variant $({ $($body)* })?)
    };
}

/// High-impact counterpart of `bt!`. The dispatcher's alone-in-turn
/// rule is enforced structurally — every variant constructed via `hi!`
/// is automatically alone-in-turn.
#[allow(unused_macros)]
macro_rules! hi {
    ($variant:ident $({ $($body:tt)* })?) => {
        Tool::HighImpact(HighImpactTool::$variant $({ $($body)* })?)
    };
}

#[test]
fn emit_package_strips_llm_supplied_output_dir_on_deserialize() {
    // C-10 / 05 regression: even when a hostile / fine-tuned LLM
    // smuggles `output_dir` into the tool-call JSON (despite the schema
    // dropping it), the `deserialize_with = "ignore_llm_output_dir"`
    // adapter zeroes it out before the dispatcher sees it. This test
    // pins the deserialization shape so a future refactor can't
    // accidentally restore the attack surface.
    //
    // `Tool` is internally tagged via `tool_name` (see the `#[serde(tag =...)]`
    // on the enum), so the variant body fields sit at the top level next
    // to `tool_name`.
    let parsed: Tool = serde_json::from_value(serde_json::json!({
        "tool_name": "emit_package",
        "output_dir": "/tmp/A; rm -rf $HOME; B"
    }))
    .expect("deserialization must succeed");
    match parsed {
        Tool::HighImpact(HighImpactTool::EmitPackage { output_dir }) => {
            assert!(
                output_dir.is_none(),
                "LLM-supplied output_dir must be stripped to None; got {output_dir:?}"
            );
        }
        other => panic!("expected EmitPackage variant, got {other:?}"),
    }
}

fn config_dir() -> PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("config")
}

fn ctx() -> ToolContext {
    ToolContext::new(config_dir(), "claude-sonnet-4-6")
}

#[tokio::test]
async fn classify_intake_returns_modality() {
    let mut s = crate::session::Session::new(false);
    let res = dispatch_one(
        &Tool::Batchable(BatchableTool::ClassifyIntake {
            prose: "single cell scRNA-seq with 10x".into(),
        }),
        &mut s,
        &ctx(),
    )
    .await;
    assert!(!res.is_error);
    assert_eq!(res.content["modality"], "single_cell_rnaseq");
}

#[tokio::test]
async fn classify_intake_rejects_empty() {
    let mut s = crate::session::Session::new(false);
    let res = dispatch_one(
        &Tool::Batchable(BatchableTool::ClassifyIntake { prose: "".into() }),
        &mut s,
        &ctx(),
    )
    .await;
    assert!(res.is_error);
    assert_eq!(res.content["error_kind"], "validation_failure");
}

#[tokio::test]
async fn classify_intake_output_omits_goal_field_when_none() {
    // Additive goal slot. The deterministic classifier
    // never authors a goal, so the serialized output suppresses the
    // field via `skip_serializing_if`. Existing fixtures that snapshot
    // classify_intake output without a goal block stay byte-stable.
    let mut s = crate::session::Session::new(false);
    let res = dispatch_one(
        &Tool::Batchable(BatchableTool::ClassifyIntake {
            prose: "single cell scRNA-seq with 10x".into(),
        }),
        &mut s,
        &ctx(),
    )
    .await;
    assert!(!res.is_error);
    assert_eq!(res.content["modality"], "single_cell_rnaseq");
    assert!(
        res.content.get("goal").is_none(),
        "deterministic classifier must not author a goal; got {:?}",
        res.content.get("goal"),
    );
}

#[test]
fn classify_intake_output_round_trips_goal_block() {
    // Round-trip test for the LLM-mediated path. When a
    // caller (future LLM extraction) populates the optional `goal:`
    // field with a shaped GoalSpec, the wrapper serializes it
    // alongside the keyword classifier's output and deserializes
    // identically. Lock in the field's shape so a refactor that
    // changes the wrapper's serde flatten semantics regresses the
    // contract.
    use super::classification::ClassifyIntakeOutput;
    use ecaa_workflow_core::classify::ClassificationResult;
    use ecaa_workflow_core::goal_spec::GoalSpec;
    use std::collections::BTreeMap;

    let goal = GoalSpec {
        edam_data: "data:3917".into(),
        edam_format: Some("format:3590".into()),
        modifiers: {
            let mut m = BTreeMap::new();
            m.insert("granularity".into(), "per-cluster".into());
            m.insert("with_marker_genes".into(), "true".into());
            m
        },
        source_prose: Some(
            "I want a clustered single-cell AnnData with marker genes per cluster.".into(),
        ),
        confidence: 0.92,
    };
    let output = ClassifyIntakeOutput {
        classification: ClassificationResult {
            modality: "single_cell_rnaseq".into(),
            taxonomy_path: "single-cell-de.yaml".into(),
            domain: String::new(),
            workflow_description: String::new(),
            edam_topic: "topic:3170".into(),
            edam_operation: "operation:3223".into(),
            confidence: 0.7,
            confidence_label: "high".into(),
            organisms: vec![],
            methods_specified: vec![],
            data_sources: vec![],
            intake_text: "I want a clustered single-cell AnnData with marker genes per cluster."
                .into(),
            goal: None,
            archetype_id: None,
            additional_modalities: vec![],
            tie_candidates: vec![],
        },
        goal: Some(goal.clone()),
    };

    // Serialize and confirm the goal block is flattened alongside the
    // classification fields at the top level.
    let json = serde_json::to_value(&output).expect("serialize ClassifyIntakeOutput");
    assert_eq!(json["modality"], "single_cell_rnaseq");
    assert_eq!(json["goal"]["edam_data"], "data:3917");
    assert_eq!(json["goal"]["edam_format"], "format:3590");
    assert_eq!(json["goal"]["modifiers"]["granularity"], "per-cluster");
    assert!((json["goal"]["confidence"].as_f64().unwrap() - 0.92).abs() < 1e-6);

    // Round-trip: a future LLM-mediated path that captures the
    // structured output and re-deserializes must reproduce the goal.
    // `ClassificationResult` doesn't impl PartialEq so compare the
    // shaped subset of fields we care about plus the GoalSpec
    // (which does impl PartialEq).
    let back: ClassifyIntakeOutput = serde_json::from_value(json).expect("round-trip");
    assert_eq!(back.classification.modality, "single_cell_rnaseq");
    assert_eq!(back.classification.taxonomy_path, "single-cell-de.yaml");
    assert_eq!(back.classification.edam_operation, "operation:3223");
    assert_eq!(back.goal, Some(goal));
}

#[tokio::test]
async fn append_intake_prose_loads_taxonomy_and_classifies() {
    let mut s = crate::session::Session::new(false);
    let res = dispatch_one(
        &Tool::Batchable(BatchableTool::AppendIntakeProse {
            prose: "single cell scRNA-seq from human IVD samples with 10x Chromium".into(),
        }),
        &mut s,
        &ctx(),
    )
    .await;
    assert!(!res.is_error, "{:?}", res);
    assert!(s.taxonomy.is_some());
    assert!(s.dag.is_some());
    // Closure Phase B.3 — `discover_*` companion synthesis now
    // surfaces a `discover_<axis>` node for every operation atom
    // with `method_choice.deferred_to` or
    // `attributes.candidate_tools`. The single_cell_de archetype
    // includes `alignment`, `batch_correction`, `clustering`, etc.,
    // each of which carries `candidate_tools` — the post-pass
    // Appends `discover_alignment`, `discover_batch_correction`,...,
    // which fires the `DagBuiltWithUnresolvedDiscovery` trigger and
    // lands the session in `IntakeFollowup`. Matches the v2 builder
    // semantics that the legacy taxonomy taxonomies encoded directly.
    assert_eq!(s.state, SessionState::IntakeFollowup);
}

#[tokio::test]
async fn append_intake_prose_advances_to_followup_when_dag_has_discovery() {
    // Closure Phase B.3 — the v4 single_cell_de archetype consumes
    // operation atoms with `candidate_tools` (alignment,
    // batch_correction, clustering, normalisation, etc.). The
    // discover-companion post-pass now synthesizes a
    // `discover_<axis>` for each, so `DagBuiltWithUnresolvedDiscovery`
    // fires and the session advances to `IntakeFollowup` after the
    // first rebuild. The second `append_intake_prose` rebuilds the
    // DAG again; the trigger fires from `IntakeFollowup` (state-
    // machine table treats it as a self-loop) so the session stays in
    // `IntakeFollowup`.
    let mut s = crate::session::Session::new(false);
    dispatch_one(
        &Tool::Batchable(BatchableTool::AppendIntakeProse {
            prose: "single cell scRNA-seq human samples".into(),
        }),
        &mut s,
        &ctx(),
    )
    .await;
    assert_eq!(s.state, SessionState::IntakeFollowup);
    dispatch_one(
        &Tool::Batchable(BatchableTool::AppendIntakeProse {
            prose: "with batch correction across 8 studies".into(),
        }),
        &mut s,
        &ctx(),
    )
    .await;
    assert_eq!(s.state, SessionState::IntakeFollowup);
}

#[tokio::test]
async fn set_intake_field_requires_taxonomy() {
    let mut s = crate::session::Session::new(false);
    let res = dispatch_one(
        &Tool::Batchable(BatchableTool::SetIntakeField {
            stage: "preprocessing".into(),
            field: "method".into(),
            value: serde_json::json!("Cell Ranger"),
        }),
        &mut s,
        &ctx(),
    )
    .await;
    assert!(res.is_error);
    assert_eq!(res.content["error_kind"], "precondition_failure");
}

#[tokio::test]
async fn set_intake_field_rejects_unknown_stage_with_alternatives() {
    let mut s = crate::session::Session::new(false);
    // Load taxonomy via append_intake_prose
    dispatch_one(
        &Tool::Batchable(BatchableTool::AppendIntakeProse {
            prose: "single cell scRNA-seq from human samples".into(),
        }),
        &mut s,
        &ctx(),
    )
    .await;

    let res = dispatch_one(
        &Tool::Batchable(BatchableTool::SetIntakeField {
            stage: "nonexistent_stage".into(),
            field: "method".into(),
            value: serde_json::json!("X"),
        }),
        &mut s,
        &ctx(),
    )
    .await;
    assert!(res.is_error);
    assert_eq!(res.content["error_kind"], "validation_failure");
    assert!(!res.content["valid_alternatives"]
        .as_array()
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn set_intake_method_rejects_unknown_stage() {
    // Closure Phase B.3 — `set_intake_method` validation requires
    // `discover_<stage>` to exist in the DAG. With B.3's
    // discover-companion synthesis, the v4 single_cell_de archetype
    // now surfaces a `discover_<axis>` for every operation atom with
    // `method_choice` / `candidate_tools` (alignment,
    // Batch_correction, clustering, cell_type_annotation,...). So
    // the rejection contract must be exercised against a stage id
    // that has no `discover_*` companion in any v4 archetype — here
    // `nonexistent_stage_for_test`.
    let mut s = crate::session::Session::new(false);
    dispatch_one(
        &Tool::Batchable(BatchableTool::AppendIntakeProse {
            prose: "single cell scRNA-seq from human IVD samples".into(),
        }),
        &mut s,
        &ctx(),
    )
    .await;
    assert!(s.dag.is_some(), "DAG must be built before the assertion");

    // Set the SME-named flag for the test stage
    // so the validation-failure path (unknown stage) is reachable. The
    // precondition gate refuses BEFORE the alternatives-list validation
    // would otherwise fire, so we need the signal to land first.
    s.sme_method_signals
        .named
        .insert("nonexistent_stage_for_test".into(), true);

    let res = dispatch_one(
        &Tool::Batchable(BatchableTool::SetIntakeMethod {
            stage: "nonexistent_stage_for_test".into(),
            method_prose: "made-up method on a stage that doesn't exist".into(),
        }),
        &mut s,
        &ctx(),
    )
    .await;
    assert!(
        res.is_error,
        "unknown stage id must fail validation; got {res:?}"
    );
    assert_eq!(res.content["error_kind"], "validation_failure");
}

#[tokio::test]
async fn set_intake_method_refuses_without_sme_signal() {
    // `set_intake_method` must refuse to fire
    // when the SME hasn't yet named a method for the target stage. The
    // LLM cannot bypass the check by calling the tool directly; the
    // UI must POST to the sme-named endpoint first.
    let mut s = crate::session::Session::new(false);
    dispatch_one(
        &Tool::Batchable(BatchableTool::AppendIntakeProse {
            prose: "single cell scRNA-seq from human IVD samples".into(),
        }),
        &mut s,
        &ctx(),
    )
    .await;
    assert!(s.dag.is_some(), "DAG must be built before the assertion");
    // No flag set: the gate must fire.
    let res = dispatch_one(
        &Tool::Batchable(BatchableTool::SetIntakeMethod {
            stage: "alignment".into(),
            method_prose: "STAR".into(),
        }),
        &mut s,
        &ctx(),
    )
    .await;
    assert!(res.is_error, "must refuse without SME signal; got {res:?}");
    assert_eq!(res.content["error_kind"], "precondition_failure");
    // No SetIntakeMethod decision should land — the precondition fires
    // BEFORE the decision-log write.
    assert!(
        !s.decisions.iter().any(|d| matches!(
            &d.decision,
            ecaa_workflow_core::decision_log::DecisionType::SetIntakeMethod { .. }
        )),
        "refused set_intake_method must NOT leave a decision-log record"
    );
}

#[tokio::test]
async fn set_intake_method_accepts_with_sme_signal() {
    // Once the SME-named flag is set, the
    // gate yields and the tool proceeds through its usual validation
    // path. This locks the gate's positive direction: a true flag
    // must not introduce any regression.
    let mut s = crate::session::Session::new(false);
    dispatch_one(
        &Tool::Batchable(BatchableTool::AppendIntakeProse {
            prose: "single cell scRNA-seq from human IVD samples".into(),
        }),
        &mut s,
        &ctx(),
    )
    .await;
    assert!(s.dag.is_some(), "DAG must be built before the assertion");
    // Server / UI posted the SME-named flag before this turn.
    s.sme_method_signals.named.insert("alignment".into(), true);
    let res = dispatch_one(
        &Tool::Batchable(BatchableTool::SetIntakeMethod {
            stage: "alignment".into(),
            method_prose: "STAR with default parameters".into(),
        }),
        &mut s,
        &ctx(),
    )
    .await;
    assert!(
        !res.is_error,
        "flagged stage must allow set_intake_method; got {res:?}"
    );
    assert!(
        s.decisions.iter().any(|d| matches!(
            &d.decision,
            ecaa_workflow_core::decision_log::DecisionType::SetIntakeMethod { stage, .. } if stage == "alignment"
        )),
        "successful set_intake_method must land a decision-log record"
    );
}

#[tokio::test]
async fn emit_package_blocked_without_user_confirmed() {
    let mut s = crate::session::Session::new(false);
    // Build DAG via append_intake_prose first
    dispatch_one(
        &Tool::Batchable(BatchableTool::AppendIntakeProse {
            prose: "single cell scRNA-seq from human samples".into(),
        }),
        &mut s,
        &ctx(),
    )
    .await;

    let tmp = tempfile::tempdir().unwrap();
    let res = dispatch_one(
        &Tool::HighImpact(HighImpactTool::EmitPackage {
            output_dir: Some(tmp.path().to_string_lossy().into_owned()),
        }),
        &mut s,
        &ctx(),
    )
    .await;
    assert!(res.is_error);
    assert_eq!(res.content["error_kind"], "precondition_failure");
}

#[tokio::test]
async fn emit_package_alone_in_batch_enforced() {
    let mut s = crate::session::Session::new(false);
    let id_a = uuid::Uuid::new_v4();
    let id_b = uuid::Uuid::new_v4();
    let batch = vec![
        (
            id_a,
            Tool::HighImpact(HighImpactTool::EmitPackage {
                output_dir: Some("/tmp/x".into()),
            }),
        ),
        (
            id_b,
            Tool::Batchable(BatchableTool::ClassifyIntake { prose: "x".into() }),
        ),
    ];
    let results = dispatch_batch(batch, &mut s, &ctx()).await;
    assert_eq!(results.len(), 2);
    for (_, r) in &results {
        assert!(r.is_error);
        assert_eq!(r.content["error_kind"], "precondition_failure");
    }
}

#[tokio::test]
async fn emit_package_is_idempotent_when_session_already_emitted() {
    // RCA: every UI session that emitted a package produced a
    // byte-identical duplicate directory ~60-70s later. The UI's
    // post-confirm `(confirmed — please continue)` follow-up turn
    // drives the LLM tool loop while the session is already in
    // Emitted (the auto-emit fired from `/confirm`), and the model
    // picks `emit_package` again. The (Emitted, EmitPackageStart) =>
    // Emitted absorption in the state machine made the second emit
    // pass; `is_confirmed()` returns true from Emitted whenever
    // `emitted_package_path` is set, so the precondition gate also
    // passed; and the handler then wrote a fresh timestamped
    // package + appended a phantom `kind: emit_package` row to
    // `runtime/decisions.jsonl`.
    //
    // Fix: the dispatch boundary short-circuits `emit_package` when
    // `state == Emitted` and a path is cached, returning the cached
    // path as a no-op success. The handler is never invoked, no new
    // directory is written, and no second decision is recorded.
    let mut s = crate::session::Session::new(false);
    // Build a DAG so any caller-side preconditions (taxonomy loaded,
    // DAG cached) would be satisfied if the handler ran.
    dispatch_one(
        &Tool::Batchable(BatchableTool::AppendIntakeProse {
            prose: "bulk rna-seq differential expression in human samples".into(),
        }),
        &mut s,
        &ctx(),
    )
    .await;

    // Drive the session into Emitted with a cached package path —
    // the exact post-`/confirm` shape produced by `try_auto_emit_after_confirm`.
    let tmp = tempfile::tempdir().unwrap();
    let cached_path: PathBuf = tmp.path().join("session-pkg-20260520T042127");
    std::fs::create_dir_all(&cached_path).unwrap();
    s.state = crate::session::SessionState::Emitted;
    s.emitted_package_path = Some(cached_path.clone());

    let decisions_before = s.decisions.len();
    // Count timestamped sibling directories before the second emit so
    // we can prove no new directory was written.
    let parent_dir = cached_path.parent().unwrap().to_path_buf();
    let dirs_before = std::fs::read_dir(&parent_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .count();

    // The model picks `emit_package` again on the auto-followup turn.
    let res = dispatch_one(
        &Tool::HighImpact(HighImpactTool::EmitPackage { output_dir: None }),
        &mut s,
        &ctx(),
    )
    .await;

    // Result is a successful no-op pointing at the cached path.
    assert!(
        !res.is_error,
        "second emit must be a no-op, not an error: {res:?}"
    );
    assert_eq!(
        res.content["output_dir"].as_str(),
        Some(cached_path.to_string_lossy().as_ref()),
        "second emit must echo the cached package path verbatim"
    );
    assert_eq!(
        res.content["noop"].as_bool(),
        Some(true),
        "second emit must be flagged noop=true so callers can distinguish"
    );

    // No new decisions row, no new directory on disk — the duplicate
    // signals the live bug used to produce.
    assert_eq!(
        s.decisions.len(),
        decisions_before,
        "no_op emit must NOT append a second `kind: emit_package` decision"
    );
    let dirs_after = std::fs::read_dir(&parent_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .count();
    assert_eq!(
        dirs_after, dirs_before,
        "no_op emit must NOT write a fresh timestamped package directory"
    );
    // Cached path itself unchanged.
    assert_eq!(s.emitted_package_path.as_ref(), Some(&cached_path));
    // State remains Emitted (the (Emitted, _) absorption preserves it,
    // and the no-op short-circuits before the post_ok hook so the
    // confirmation token's `consume()` is not double-invoked).
    assert!(matches!(s.state, crate::session::SessionState::Emitted));
}

#[tokio::test]
async fn dispatch_batch_records_audit_log() {
    let mut s = crate::session::Session::new(false);
    let id = uuid::Uuid::new_v4();
    let batch = vec![(
        id,
        Tool::Batchable(BatchableTool::ClassifyIntake {
            prose: "rnaseq".into(),
        }),
    )];
    let _ = dispatch_batch(batch, &mut s, &ctx()).await;
    assert_eq!(s.tool_call_log.len(), 1);
    assert_eq!(s.tool_call_log[0].tool_name, "classify_intake");
    assert_eq!(s.tool_call_log[0].turn_id, id);
}

#[tokio::test]
async fn append_intake_prose_generic_fallback_stays_unblocked() {
    // Totally off-topic prose routes to generic_omics @ 0% confidence.
    // The floor should NOT block the fallback — generic_omics exists
    // precisely to catch unknown-shape work.
    let mut s = crate::session::Session::new(false);
    let res = dispatch_one(
        &Tool::Batchable(BatchableTool::AppendIntakeProse {
            prose: "help me pick a restaurant for dinner".into(),
        }),
        &mut s,
        &ctx(),
    )
    .await;
    assert!(!res.is_error, "{:?}", res);
    assert_eq!(res.content["modality"].as_str(), Some("generic_omics"));
    assert!(
        !matches!(s.state, crate::session::SessionState::Blocked { .. }),
        "fallback should stay unblocked; got {:?}",
        s.state,
    );
}

#[tokio::test]
async fn propose_summary_confirmation_advances_state() {
    let mut s = crate::session::Session::new(false);
    dispatch_one(
        &Tool::Batchable(BatchableTool::AppendIntakeProse {
            prose: "bulk rna-seq differential expression in human samples".into(),
        }),
        &mut s,
        &ctx(),
    )
    .await;
    let res = dispatch_one(
        &Tool::Batchable(BatchableTool::ProposeSummaryConfirmation {
            summary_markdown: "Here is the plan…".into(),
        }),
        &mut s,
        &ctx(),
    )
    .await;
    assert!(!res.is_error);
    assert_eq!(s.state, SessionState::PendingConfirmation { stage: None });
}

#[tokio::test]
async fn propose_quick_replies_validates_options() {
    let mut s = crate::session::Session::new(false);
    let res = dispatch_one(
        &Tool::Batchable(BatchableTool::ProposeQuickReplies {
            question: "?".into(),
            options: vec![],
        }),
        &mut s,
        &ctx(),
    )
    .await;
    assert!(res.is_error);
}

#[tokio::test]
async fn amend_stage_method_rejects_empty_method_prose() {
    let mut s = crate::session::Session::new(false);
    let res = dispatch_one(
        &Tool::HighImpact(HighImpactTool::AmendStageMethod {
            stage: "x".into(),
            method_prose: "   ".into(),
            rationale: None,
        }),
        &mut s,
        &ctx(),
    )
    .await;
    assert!(res.is_error);
    assert_eq!(res.content["error_kind"], "validation_failure");
}

#[tokio::test]
async fn amend_stage_method_requires_emitted_state() {
    let mut s = crate::session::Session::new(false);
    dispatch_one(
        &Tool::Batchable(BatchableTool::AppendIntakeProse {
            prose: "bulk rna-seq differential expression in human samples".into(),
        }),
        &mut s,
        &ctx(),
    )
    .await;
    // State is Intake/IntakeFollowup, not Emitted — tool must reject.
    let res = dispatch_one(
        &Tool::HighImpact(HighImpactTool::AmendStageMethod {
            stage: "differential_expression".into(),
            method_prose: "limma-voom".into(),
            rationale: None,
        }),
        &mut s,
        &ctx(),
    )
    .await;
    assert!(res.is_error);
    assert_eq!(res.content["error_kind"], "precondition_failure");
}

#[tokio::test]
async fn amend_stage_method_rejects_unknown_stage() {
    let mut s = crate::session::Session::new(false);
    dispatch_one(
        &Tool::Batchable(BatchableTool::AppendIntakeProse {
            prose: "bulk rna-seq differential expression in human samples".into(),
        }),
        &mut s,
        &ctx(),
    )
    .await;
    // Force state to Emitted so only the stage-existence check fires.
    s.state = crate::session::SessionState::Emitted;
    let res = dispatch_one(
        &Tool::HighImpact(HighImpactTool::AmendStageMethod {
            stage: "not_a_real_stage".into(),
            method_prose: "anything".into(),
            rationale: None,
        }),
        &mut s,
        &ctx(),
    )
    .await;
    assert!(res.is_error);
    assert_eq!(res.content["error_kind"], "validation_failure");
    assert!(!res.content["valid_alternatives"]
        .as_array()
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn amend_stage_method_invalidates_slice_and_advances_to_ready_to_emit() {
    let mut s = crate::session::Session::new(false);
    dispatch_one(
        &Tool::Batchable(BatchableTool::AppendIntakeProse {
            prose: "bulk rna-seq differential expression in human samples".into(),
        }),
        &mut s,
        &ctx(),
    )
    .await;
    s.state = crate::session::SessionState::Emitted;

    // Pick a stage id that definitely exists in the built DAG.
    let target: String = s
        .dag
        .as_ref()
        .unwrap()
        .tasks
        .keys()
        .find(|k| {
            k.as_str().starts_with("differential_expression")
                || k.as_str().starts_with("normalization")
        })
        .map(|k| k.to_string())
        .unwrap_or_else(|| {
            s.dag
                .as_ref()
                .unwrap()
                .tasks
                .keys()
                .next()
                .map(|k| k.to_string())
                .unwrap()
        });

    let res = dispatch_one(
        &Tool::HighImpact(HighImpactTool::AmendStageMethod {
            stage: target.clone(),
            method_prose: "limma-voom with quality weights".into(),
            rationale: None,
        }),
        &mut s,
        &ctx(),
    )
    .await;
    assert!(!res.is_error, "{:?}", res);
    assert_eq!(
        res.content["stage"],
        serde_json::Value::String(target.clone())
    );
    assert!(!res.content["invalidated_tasks"]
        .as_array()
        .unwrap()
        .is_empty());
    assert_eq!(s.state, crate::session::SessionState::ReadyToEmit);
}

#[tokio::test]
async fn branch_session_rejects_pre_intake() {
    let s = crate::session::Session::new(false);
    // Greeting state — no intake started yet.
    let mut s_mut = s.clone();
    let res = dispatch_one(
        &Tool::HighImpact(HighImpactTool::BranchSession { rationale: None }),
        &mut s_mut,
        &ctx(),
    )
    .await;
    assert!(res.is_error);
    assert_eq!(res.content["error_kind"], "precondition_failure");
}

// ── Confirmatory-mode deviation gate ────────────────────────────

#[tokio::test]
async fn confirmatory_amend_of_prespecified_stage_requires_rationale() {
    let mut s = crate::session::Session::new(false);
    dispatch_one(
        &Tool::Batchable(BatchableTool::AppendIntakeProse {
            prose: "bulk rna-seq differential expression in human samples".into(),
        }),
        &mut s,
        &ctx(),
    )
    .await;
    s.state = crate::session::SessionState::Emitted;
    let target = s
        .dag
        .as_ref()
        .and_then(|d| d.tasks.keys().next().cloned())
        .unwrap();
    s.mode = ecaa_workflow_core::session_mode::SessionMode::Confirmatory {
        prespecified_stages: vec![target.to_string()],
        prespecified_parameters: std::collections::BTreeMap::new(),
    };
    s.mode_locked = true;
    // No rationale → RationaleRequired.
    let res = dispatch_one(
        &Tool::HighImpact(HighImpactTool::AmendStageMethod {
            stage: target.to_string(),
            method_prose: "alternative method".into(),
            rationale: None,
        }),
        &mut s,
        &ctx(),
    )
    .await;
    assert!(res.is_error);
    assert_eq!(res.content["error_kind"], "rationale_required");
}

#[tokio::test]
async fn confirmatory_amend_with_rationale_writes_post_hoc_deviation_record() {
    let mut s = crate::session::Session::new(false);
    dispatch_one(
        &Tool::Batchable(BatchableTool::AppendIntakeProse {
            prose: "bulk rna-seq differential expression in human samples".into(),
        }),
        &mut s,
        &ctx(),
    )
    .await;
    s.state = crate::session::SessionState::Emitted;
    let target = s
        .dag
        .as_ref()
        .and_then(|d| d.tasks.keys().next().cloned())
        .unwrap();
    s.mode = ecaa_workflow_core::session_mode::SessionMode::Confirmatory {
        prespecified_stages: vec![target.to_string()],
        prespecified_parameters: std::collections::BTreeMap::new(),
    };
    s.mode_locked = true;
    let before = s.decisions.len();
    let res = dispatch_one(
        &Tool::HighImpact(HighImpactTool::AmendStageMethod {
            stage: target.to_string(),
            method_prose: "alternative method".into(),
            rationale: Some("site imbalance in the primary analysis set".into()),
        }),
        &mut s,
        &ctx(),
    )
    .await;
    assert!(!res.is_error, "{:?}", res);
    // One AmendStage + one PostHocDeviation = 2 new records.
    assert_eq!(s.decisions.len(), before + 2);
    let has_deviation = s.decisions.iter().any(|d| {
        matches!(
            d.decision,
            ecaa_workflow_core::decision_log::DecisionType::PostHocDeviation { .. }
        )
    });
    assert!(has_deviation, "expected a PostHocDeviation record");
}

#[tokio::test]
async fn exploratory_amend_never_writes_post_hoc_deviation() {
    let mut s = crate::session::Session::new(false);
    dispatch_one(
        &Tool::Batchable(BatchableTool::AppendIntakeProse {
            prose: "bulk rna-seq differential expression in human samples".into(),
        }),
        &mut s,
        &ctx(),
    )
    .await;
    s.state = crate::session::SessionState::Emitted;
    // Default mode is Exploratory — no deviation gate fires regardless
    // of whether a rationale is supplied.
    let target = s
        .dag
        .as_ref()
        .and_then(|d| d.tasks.keys().next().cloned())
        .unwrap();
    let res = dispatch_one(
        &Tool::HighImpact(HighImpactTool::AmendStageMethod {
            stage: target.to_string(),
            method_prose: "alternative method".into(),
            rationale: Some("ignored in exploratory mode".into()),
        }),
        &mut s,
        &ctx(),
    )
    .await;
    assert!(!res.is_error);
    let any_deviation = s.decisions.iter().any(|d| {
        matches!(
            d.decision,
            ecaa_workflow_core::decision_log::DecisionType::PostHocDeviation { .. }
        )
    });
    assert!(!any_deviation);
}

#[tokio::test]
async fn branch_session_returns_parent_metadata() {
    let mut s = crate::session::Session::new(false);
    dispatch_one(
        &Tool::Batchable(BatchableTool::AppendIntakeProse {
            prose: "bulk rna-seq differential expression in human samples".into(),
        }),
        &mut s,
        &ctx(),
    )
    .await;
    let parent_id = s.id.to_string();
    let res = dispatch_one(
        &Tool::HighImpact(HighImpactTool::BranchSession {
            rationale: Some("explore an alternative method".into()),
        }),
        &mut s,
        &ctx(),
    )
    .await;
    assert!(!res.is_error, "{:?}", res);
    assert_eq!(res.content["parent_session_id"], parent_id);
    assert_eq!(res.content["rationale"], "explore an alternative method");
}

#[tokio::test]
async fn branch_session_rejects_emitting_state() {
    let mut s = crate::session::Session::new(false);
    dispatch_one(
        &Tool::Batchable(BatchableTool::AppendIntakeProse {
            prose: "bulk rna-seq differential expression in human samples".into(),
        }),
        &mut s,
        &ctx(),
    )
    .await;
    s.state = crate::session::SessionState::Emitting;
    let res = dispatch_one(
        &Tool::HighImpact(HighImpactTool::BranchSession { rationale: None }),
        &mut s,
        &ctx(),
    )
    .await;
    assert!(res.is_error);
    assert_eq!(res.content["error_kind"], "precondition_failure");
}

#[tokio::test]
async fn rerun_task_without_recorded_method_errors() {
    let mut s = crate::session::Session::new(false);
    dispatch_one(
        &Tool::Batchable(BatchableTool::AppendIntakeProse {
            prose: "bulk rna-seq differential expression in human samples".into(),
        }),
        &mut s,
        &ctx(),
    )
    .await;
    s.state = crate::session::SessionState::Emitted;
    // Call rerun_task on a task that exists but has no recorded method.
    let target: String = s
        .dag
        .as_ref()
        .unwrap()
        .tasks
        .keys()
        .next()
        .map(|k| k.to_string())
        .unwrap();
    let res = dispatch_one(
        &Tool::HighImpact(HighImpactTool::RerunTask {
            task_id: target,
            reason: None,
        }),
        &mut s,
        &ctx(),
    )
    .await;
    assert!(res.is_error);
    assert_eq!(res.content["error_kind"], "precondition_failure");
}

#[tokio::test]
async fn rerun_task_with_recorded_method_wraps_amend() {
    let mut s = crate::session::Session::new(false);
    dispatch_one(
        &Tool::Batchable(BatchableTool::AppendIntakeProse {
            prose: "bulk rna-seq differential expression in human samples".into(),
        }),
        &mut s,
        &ctx(),
    )
    .await;
    let target: String = s
        .dag
        .as_ref()
        .unwrap()
        .tasks
        .keys()
        .next()
        .map(|k| k.to_string())
        .unwrap();
    // Record a method so rerun has something to rerun with.
    s.intake_methods
        .set(&target, Some("deseq2 standard pipeline".into()), None);
    s.state = crate::session::SessionState::Emitted;
    let res = dispatch_one(
        &Tool::HighImpact(HighImpactTool::RerunTask {
            task_id: target.clone(),
            reason: Some("input data refreshed".into()),
        }),
        &mut s,
        &ctx(),
    )
    .await;
    assert!(!res.is_error, "{:?}", res);
    assert_eq!(res.content["rerun"], true);
    assert_eq!(res.content["rerun_reason"], "input data refreshed");
    assert_eq!(s.state, crate::session::SessionState::ReadyToEmit);
}

#[tokio::test]
async fn rerun_task_is_alone_in_turn() {
    let mut s = crate::session::Session::new(false);
    dispatch_one(
        &Tool::Batchable(BatchableTool::AppendIntakeProse {
            prose: "bulk rna-seq differential expression in human samples".into(),
        }),
        &mut s,
        &ctx(),
    )
    .await;
    let target: String = s
        .dag
        .as_ref()
        .unwrap()
        .tasks
        .keys()
        .next()
        .map(|k| k.to_string())
        .unwrap();
    let batch = vec![
        (
            Uuid::new_v4(),
            Tool::HighImpact(HighImpactTool::RerunTask {
                task_id: target,
                reason: None,
            }),
        ),
        (Uuid::new_v4(), bt!(GetSessionState)),
    ];
    let results = dispatch_batch(batch, &mut s, &ctx()).await;
    assert_eq!(results.len(), 2);
    assert!(results.iter().all(|(_, r)| r.is_error));
}

#[tokio::test]
async fn select_sensitivity_winner_requires_awaiting_selection_state() {
    let mut s = crate::session::Session::new(false);
    let res = dispatch_one(
        &Tool::HighImpact(HighImpactTool::SelectSensitivityWinner {
            stage: "x".into(),
            winner: "y".into(),
            rationale: None,
        }),
        &mut s,
        &ctx(),
    )
    .await;
    assert!(res.is_error);
    assert_eq!(res.content["error_kind"], "precondition_failure");
}

#[tokio::test]
async fn select_sensitivity_winner_rejects_non_candidate() {
    use ecaa_workflow_core::blocker::{BlockerContext, BlockerKind};
    let mut s = crate::session::Session::new(false);
    dispatch_one(
        &Tool::Batchable(BatchableTool::AppendIntakeProse {
            prose: "bulk rna-seq differential expression in human samples".into(),
        }),
        &mut s,
        &ctx(),
    )
    .await;
    // Force Blocked{AwaitingSmeSelection}.
    s.state = crate::session::SessionState::Blocked {
        blockers: vec![],
        reason: "awaiting integration selection".into(),
        recovery_hint: "pick one".into(),
        blocker_kind: Some(BlockerKind::AwaitingSmeSelection {
            stage_id: "compare_integration".into(),
            candidates: vec!["harmony".into(), "scanorama".into(), "bbknn".into()],
        }),
        context: Some(BlockerContext {
            timestamp: "2026-04-16T00:00:00Z".into(),
            recovery_hints: None,
        }),
    };
    let res = dispatch_one(
        &Tool::HighImpact(HighImpactTool::SelectSensitivityWinner {
            stage: "compare_integration".into(),
            winner: "nonexistent_method".into(),
            rationale: None,
        }),
        &mut s,
        &ctx(),
    )
    .await;
    assert!(res.is_error);
    assert_eq!(res.content["error_kind"], "validation_failure");
    let alts = res.content["valid_alternatives"].as_array().unwrap();
    assert_eq!(alts.len(), 3);
}

#[tokio::test]
async fn select_sensitivity_winner_records_and_unblocks() {
    use ecaa_workflow_core::blocker::{BlockerContext, BlockerKind};
    let mut s = crate::session::Session::new(false);
    dispatch_one(
        &Tool::Batchable(BatchableTool::AppendIntakeProse {
            prose: "bulk rna-seq differential expression in human samples".into(),
        }),
        &mut s,
        &ctx(),
    )
    .await;
    // Pick any real stage id so the DAG contains the amended target.
    let target: String = s
        .dag
        .as_ref()
        .unwrap()
        .tasks
        .keys()
        .next()
        .map(|k| k.to_string())
        .unwrap();
    s.state = crate::session::SessionState::Blocked {
        blockers: vec![],
        reason: "awaiting selection".into(),
        recovery_hint: "pick one".into(),
        blocker_kind: Some(BlockerKind::AwaitingSmeSelection {
            stage_id: target.clone(),
            candidates: vec!["harmony".into(), "scanorama".into(), "bbknn".into()],
        }),
        context: Some(BlockerContext {
            timestamp: "2026-04-16T00:00:00Z".into(),
            recovery_hints: None,
        }),
    };
    let res = dispatch_one(
        &Tool::HighImpact(HighImpactTool::SelectSensitivityWinner {
            stage: target.clone(),
            winner: "harmony".into(),
            rationale: Some("tightest silhouette".into()),
        }),
        &mut s,
        &ctx(),
    )
    .await;
    assert!(!res.is_error, "{:?}", res);
    assert_eq!(res.content["winner"], "harmony");
    // Winner recorded in intake_methods.
    assert_eq!(
        s.intake_methods.0.get(&target).map(|r| r.method.as_str()),
        Some("harmony")
    );
    // Session is back in the intake flow (Intake or IntakeFollowup
    // depending on whether rebuild_dag's discovery-check fired).
    assert!(
        matches!(
            s.state,
            crate::session::SessionState::Intake | crate::session::SessionState::IntakeFollowup
        ),
        "expected Intake|IntakeFollowup, got {:?}",
        s.state
    );
}

#[tokio::test]
async fn amend_is_alone_in_turn() {
    let mut s = crate::session::Session::new(false);
    dispatch_one(
        &Tool::Batchable(BatchableTool::AppendIntakeProse {
            prose: "bulk rna-seq differential expression in human samples".into(),
        }),
        &mut s,
        &ctx(),
    )
    .await;
    s.state = crate::session::SessionState::Emitted;
    // Amend + something else must reject with precondition failure.
    let target: String = s
        .dag
        .as_ref()
        .unwrap()
        .tasks
        .keys()
        .next()
        .map(|k| k.to_string())
        .unwrap();
    let batch = vec![
        (
            Uuid::new_v4(),
            Tool::HighImpact(HighImpactTool::AmendStageMethod {
                stage: target,
                method_prose: "anything".into(),
                rationale: None,
            }),
        ),
        (Uuid::new_v4(), bt!(GetSessionState)),
    ];
    let results = dispatch_batch(batch, &mut s, &ctx()).await;
    assert_eq!(results.len(), 2);
    assert!(results.iter().all(|(_, r)| r.is_error));
}

#[tokio::test]
async fn get_task_result_without_dag_is_precondition_failure() {
    let mut s = crate::session::Session::new(false);
    let res = dispatch_one(
        &Tool::Batchable(BatchableTool::GetTaskResult {
            task_id: "any".into(),
        }),
        &mut s,
        &ctx(),
    )
    .await;
    assert!(res.is_error);
    assert_eq!(res.content["error_kind"], "precondition_failure");
}

#[tokio::test]
async fn get_task_result_for_unknown_task_suggests_alternatives() {
    let mut s = crate::session::Session::new(false);
    dispatch_one(
        &Tool::Batchable(BatchableTool::AppendIntakeProse {
            prose: "bulk rna-seq differential expression in human samples".into(),
        }),
        &mut s,
        &ctx(),
    )
    .await;
    let res = dispatch_one(
        &Tool::Batchable(BatchableTool::GetTaskResult {
            task_id: "does_not_exist".into(),
        }),
        &mut s,
        &ctx(),
    )
    .await;
    assert!(res.is_error);
    assert_eq!(res.content["error_kind"], "validation_failure");
    let alts = res.content["valid_alternatives"].as_array().unwrap();
    assert!(!alts.is_empty(), "must list candidate task ids");
}

#[tokio::test]
async fn get_task_result_for_pending_task_reports_precondition() {
    let mut s = crate::session::Session::new(false);
    dispatch_one(
        &Tool::Batchable(BatchableTool::AppendIntakeProse {
            prose: "bulk rna-seq differential expression in human samples".into(),
        }),
        &mut s,
        &ctx(),
    )
    .await;
    // Pick any task that's Pending / Ready and call get_task_result on it.
    let first_pending = s
        .dag
        .as_ref()
        .unwrap()
        .tasks
        .iter()
        .find(|(_, t)| {
            matches!(
                t.state,
                ecaa_workflow_core::dag::TaskState::Pending
                    | ecaa_workflow_core::dag::TaskState::Ready
            )
        })
        .map(|(id, _)| id.to_string())
        .expect("a freshly-built DAG has at least one Pending/Ready task");
    let res = dispatch_one(
        &Tool::Batchable(BatchableTool::GetTaskResult {
            task_id: first_pending,
        }),
        &mut s,
        &ctx(),
    )
    .await;
    assert!(res.is_error);
    assert_eq!(res.content["error_kind"], "precondition_failure");
}

#[tokio::test]
async fn push_with_rotation_drops_oldest_above_cap() {
    // the in-memory tool_call_log caps at TOOL_CALL_LOG_CAP.
    // Beyond that, the oldest record is evicted so the length stays
    // bounded. Without an emitted package, the evicted record is
    // dropped silently (decisions.jsonl is the long-term audit).
    let mut s = Session::new(false);
    let record = || ToolCallRecord {
        turn_id: Uuid::new_v4(),
        tool_name: "test".into(),
        args: serde_json::Value::Null,
        result: serde_json::Value::Null,
        is_error: false,
        model: "sonnet-4-6".to_string(),
        timestamp: chrono::Utc::now(),
    };
    for _ in 0..TOOL_CALL_LOG_CAP + 5 {
        push_with_rotation(&mut s, record());
    }
    assert_eq!(s.tool_call_log.len(), TOOL_CALL_LOG_CAP);
}

#[tokio::test]
async fn push_with_rotation_rotates_overflow_to_jsonl_when_emitted() {
    // for emitted sessions, evictions append to
    // `runtime/tool_call_log.jsonl` so the audit stays complete.
    let tmp = tempfile::tempdir().unwrap();
    let mut s = Session::new(false);
    s.emitted_package_path = Some(tmp.path().to_path_buf());
    let record = || ToolCallRecord {
        turn_id: Uuid::new_v4(),
        tool_name: "test".into(),
        args: serde_json::Value::Null,
        result: serde_json::Value::Null,
        is_error: false,
        model: "sonnet-4-6".to_string(),
        timestamp: chrono::Utc::now(),
    };
    for _ in 0..TOOL_CALL_LOG_CAP + 3 {
        push_with_rotation(&mut s, record());
    }
    assert_eq!(s.tool_call_log.len(), TOOL_CALL_LOG_CAP);
    let jsonl = tmp.path().join("runtime/tool_call_log.jsonl");
    let content = std::fs::read_to_string(&jsonl).expect("overflow jsonl exists");
    let lines: Vec<&str> = content.lines().collect();
    assert_eq!(
        lines.len(),
        3,
        "expected 3 rotated lines, got {}",
        lines.len()
    );
}

// ── ToolSpec metadata table ─────────────────────────────────────────
//
// The compiler's exhaustive `match` in `Tool::spec` already guarantees
// every variant has a row. The tests below additionally pin: (a) every
// name is unique + non-empty, (b) the alone-in-turn set hasn't
// silently grown or shrunk — adding a tool must explicitly opt into
// the alone-in-turn set in this test, and (c) the two bucket enums
// partition cleanly (their counts sum to `Tool::COUNT`).

fn every_tool_variant() -> Vec<Tool> {
    // Source of truth lives on the bucket enums so we can't drift
    // between this list and the JSON-schema list.
    Tool::all_variants_for_tests()
}

#[test]
fn every_tool_variant_has_unique_non_empty_name() {
    let mut seen = std::collections::HashSet::new();
    for tool in every_tool_variant() {
        let name = tool.name();
        assert!(!name.is_empty(), "{:?} has empty name", tool);
        assert!(
            seen.insert(name),
            "duplicate tool name: {} (variant {:?})",
            name,
            tool
        );
    }
    assert_eq!(seen.len(), 22, "expected 22 distinct tool names");
}

#[test]
fn alone_in_turn_set_is_pinned() {
    let mut alone: Vec<&'static str> = every_tool_variant()
        .iter()
        .filter(|t| t.is_alone_in_turn())
        .map(|t| t.name())
        .collect();
    alone.sort();
    assert_eq!(
        alone,
        vec![
            "amend_stage_method",
            "branch_session",
            "emit_package",
            "propose_hypothesized_node",
            "propose_hypothesized_renderer",
            "rerun_task",
            "select_sensitivity_winner",
            "start_execution",
        ],
        "alone-in-turn set drifted — adding a tool must explicitly extend this list"
    );
}

#[test]
fn mutation_set_is_pinned() {
    let mut muts: Vec<&'static str> = every_tool_variant()
        .iter()
        .filter(|t| t.is_mutation())
        .map(|t| t.name())
        .collect();
    muts.sort();
    assert_eq!(
        muts,
        vec![
            "amend_stage_method",
            "append_intake_prose",
            "branch_session",
            "emit_package",
            "propose_hypothesized_node",
            "propose_hypothesized_renderer",
            "propose_summary_confirmation",
            "rerun_task",
            "select_sensitivity_winner",
            "set_intake_excluded_atoms",
            "set_intake_field",
            "set_intake_method",
            "set_intake_modality",
            "start_execution",
        ],
        "mutation set drifted — adding a tool must explicitly extend this list"
    );
}

// ── R-2 / C20 bucket-enum invariants ────────────────────────────────
//
// These compile-time-anchored assertions lock the structural alone-in-
// turn rule: the 8 high-impact tools must live in `HighImpactTool` and
// nowhere else; the remaining 12 must live in `BatchableTool`. A
// future variant that accidentally lands in the wrong bucket fails
// the count match here in addition to the dispatcher's runtime check.

#[test]
fn high_impact_tools_count_matches_documented_8() {
    use strum::EnumCount;
    assert_eq!(
        <super::HighImpactTool as EnumCount>::COUNT,
        8,
        "CLAUDE.md pins the high-impact bucket at exactly 8 tools: \
         emit_package, amend_stage_method, rerun_task, \
         select_sensitivity_winner, branch_session, start_execution, \
         propose_hypothesized_node, propose_hypothesized_renderer"
    );
}

#[test]
fn batchable_tools_count_matches_documented_13() {
    use strum::EnumCount;
    assert_eq!(
        <super::BatchableTool as EnumCount>::COUNT,
        14,
        "Batchable bucket is the residual after the 8 high-impact split: \
         7 read-only (classify_intake, get_taxonomy_info, get_session_state, \
         get_classification_evidence, get_task_result, get_literature_context, \
         list_atoms) + 5 intake-mutation (set_intake_field, set_intake_method, \
         set_intake_excluded_atoms, set_intake_modality, append_intake_prose) \
         + 2 conversational (propose_summary_confirmation, propose_quick_replies)"
    );
}

#[test]
fn all_tools_partition_cleanly() {
    use strum::EnumCount;
    assert_eq!(
        Tool::COUNT,
        <super::BatchableTool as EnumCount>::COUNT + <super::HighImpactTool as EnumCount>::COUNT,
        "Tool::COUNT must equal BatchableTool::COUNT + HighImpactTool::COUNT — \
         the two bucket enums must partition the closed vocabulary with no \
         overlap and no orphan variant on Tool itself"
    );
}

#[test]
fn batchable_variants_are_never_alone_in_turn() {
    // Structural guarantee — `Tool::is_alone_in_turn()` returns true
    // iff the wrapper is `Tool::HighImpact(_)`. Iterating the Batchable
    // bucket should never produce an alone-in-turn tool. A regression
    // here would mean the wrapper itself is broken.
    for t in super::BatchableTool::all_variants_for_tests() {
        let wrapped: Tool = t.into();
        assert!(
            !wrapped.is_alone_in_turn(),
            "BatchableTool variant {} must not be alone-in-turn",
            wrapped.name()
        );
    }
}

#[test]
fn high_impact_variants_are_always_alone_in_turn() {
    for t in super::HighImpactTool::all_variants_for_tests() {
        let wrapped: Tool = t.into();
        assert!(
            wrapped.is_alone_in_turn(),
            "HighImpactTool variant {} must be alone-in-turn",
            wrapped.name()
        );
    }
}

// ── Clinical-trial class end-to-end via append_intake_prose ────

// ── Checkpoint modes ────────────────────────────────────────

#[test]
fn checkpoint_mode_fast_auto_advances_everything() {
    use ecaa_workflow_core::checkpoint_mode::CheckpointMode;
    let mode = CheckpointMode::Fast;
    assert!(mode.auto_advances(true));
    assert!(mode.auto_advances(false));
}

#[test]
fn checkpoint_mode_selective_auto_advances_only_non_required() {
    use ecaa_workflow_core::checkpoint_mode::CheckpointMode;
    let mode = CheckpointMode::Selective;
    assert!(mode.auto_advances(false));
    assert!(!mode.auto_advances(true));
}

#[tokio::test]
async fn time_series_prose_loads_time_series_taxonomy() {
    let mut s = crate::session::Session::new(false);
    dispatch_one(
        &Tool::Batchable(BatchableTool::AppendIntakeProse {
            prose: "SARIMA time series forecast of monthly hospital admissions \
                    with 12-month forecast horizon, ADF stationarity check, and \
                    Ljung-Box residual diagnostics; report RMSE and MAPE."
                .into(),
        }),
        &mut s,
        &ctx(),
    )
    .await;
    assert_eq!(
        s.project_class,
        ecaa_workflow_core::project_class::ProjectClass::TimeSeriesForecast,
        "time-series vocabulary must route to TimeSeriesForecast"
    );
    let taxonomy = s.taxonomy.as_ref().expect("time-series taxonomy loaded");
    // Phase B4 — archetype id is snake_case.
    assert_eq!(taxonomy.id, "time_series_forecast");
    // Phase B4 — `time_series_forecast.yaml` archetype does not declare
    // `preferred_container`, so the metadata holder carries None. The
    // legacy taxonomy YAML pinned `scripps/bioinformatics:1.0` per the
    // Unified-image directive; that pinning is out of scope
    // for B4 (archetype-catalog work).
    assert_eq!(taxonomy.preferred_container.as_deref(), None);
    // DAG composition still happens via v4; the test crate registry
    // load may produce different stage ids than the legacy taxonomy
    // (the time_series_forecast archetype's atom list is
    // [time_series_decompose, time_series_model_fit,
    // time_series_forecast_evaluate]).
    let dag = s.dag.as_ref().expect("DAG built");
    assert!(
        !dag.tasks.is_empty(),
        "v4 composer should produce a non-empty DAG for time-series intake"
    );
}

#[tokio::test]
async fn clinical_trial_prose_loads_clinical_taxonomy() {
    let mut s = crate::session::Session::new(false);
    dispatch_one(
        &Tool::Batchable(BatchableTool::AppendIntakeProse {
            prose: "Phase III randomized controlled trial of Drug X vs placebo; \
                    frozen SAP; ITT is the primary analysis set; primary endpoint is \
                    ACR20 response at Week 24 analyzed by logistic regression; \
                    secondary endpoints use MMRM."
                .into(),
        }),
        &mut s,
        &ctx(),
    )
    .await;
    assert_eq!(
        s.project_class,
        ecaa_workflow_core::project_class::ProjectClass::ClinicalTrial,
        "clinical vocabulary must route to ClinicalTrial"
    );
    let taxonomy = s.taxonomy.as_ref().expect("clinical-trial taxonomy loaded");
    // Phase B4 — `Session.taxonomy.id` is now populated from the
    // matched archetype id, which uses snake_case (was kebab-case in
    // the deleted YAML taxonomy filenames).
    assert_eq!(taxonomy.id, "clinical_trial_analysis");
    // `preferred_container` comes from the archetype's
    // `preferred_container.image` field. `clinical_trial_analysis.yaml`
    // currently pins `rocker/r-ver` (no unbounded egress posture for
    // clinical-trial agents; see the file's preferred_container comment
    // for the PCCP rationale). When the archetype's image changes,
    // update this expectation in lockstep — the test is checking that
    // the loader threads the value through, not which specific image
    // is correct.
    assert_eq!(
        taxonomy.preferred_container.as_deref(),
        Some("rocker/r-ver")
    );
    // DAG is now built via the `clinical_trial_analysis`
    // Archetype rather than the legacy taxonomy. update —
    // the archetype's atom list now uses the clinical-trial-specific
    // analysis atoms (`clinical_endpoint_analysis`,
    // `clinical_safety_summary`, `clinical_subgroup_analysis`,
    // `clinical_sensitivity_analysis`) instead of the RNA-seq
    // `qc_preprocessing` + `differential_expression` chain; the legacy
    // chain couldn't accept the CDISC ADaM tabular shape produced by
    // `data_import` and tripped a `GoalUnreachable` in the v4 planner.
    // Pre-Phase-6.0 the DAG had legacy stage names (`cdisc_mapping`,
    // `primary_endpoint`, etc.) from `clinical-trial-analysis.yaml`.
    let dag = s.dag.as_ref().expect("DAG built");
    assert!(
        dag.tasks.contains_key("data_import"),
        "clinical-trial archetype DAG must contain data_import; got {:?}",
        dag.tasks.keys().collect::<Vec<_>>()
    );
    assert!(
        dag.tasks.contains_key("clinical_endpoint_analysis"),
        "clinical-trial archetype DAG must contain clinical_endpoint_analysis; got {:?}",
        dag.tasks.keys().collect::<Vec<_>>()
    );
    assert!(dag.tasks.contains_key("reporting"));
    assert!(dag.tasks.contains_key("final_reporting"));
}

/// `append_intake_prose` populates `session.archetype_snapshot` when
/// the classifier's `goal_patterns:` block matched a goal AND the
/// archetype catalog has a clear winner (outside the 5%-tie margin).
/// Validates the production archetype-detection wiring end-to-end so
/// the composer fast-path can route through it.
/// Uses variant-calling prose because variant-calling has a unique
/// goal triple (data:3498 / format:3016) and `variant_calling_germline`
/// is the sole bioinformatics archetype scoring on it; this avoids
/// the cross-modality DE tie (data:0951 + format:3475 is the goal of
/// bulk_rnaseq_de + long_read_rnaseq + metagenomics_taxonomic, all
/// scored 6, suppressed by the 5%-tie window).
#[tokio::test]
async fn append_intake_prose_snapshots_archetype_when_goal_matches_unique_winner() {
    let mut s = crate::session::Session::new(false);
    let res = dispatch_one(
        &Tool::Batchable(BatchableTool::AppendIntakeProse {
            prose: "germline variant calling on whole-genome sequencing with deepvariant".into(),
        }),
        &mut s,
        &ctx(),
    )
    .await;
    assert!(!res.is_error, "{:?}", res);

    // The test prose hits the variant_calling goal pattern
    // (`data:3498` / `format:3016`); only `variant_calling_germline`
    // archetype scores on it.
    if s.classification
        .as_ref()
        .and_then(|c| c.goal.as_ref())
        .is_some()
    {
        // Diagnostics: dump the live archetype-match scoring so
        // future changes that regress the wiring report the
        // precise gap.
        let goal = s.classification.as_ref().unwrap().goal.as_ref().unwrap();
        let archetype_dir = config_dir().join("archetypes");
        let reg = ecaa_workflow_core::archetype_registry::ArchetypeRegistry::load_from_dir(
            &archetype_dir,
        )
        .expect("load archetype registry from config");
        let matches = reg.find_match(
            &goal.edam_data,
            goal.edam_format.as_deref(),
            "bioinformatics",
        );
        assert!(
            !matches.is_empty(),
            "find_match must return at least one candidate for goal {:?} / project_class bioinformatics, got 0",
            goal
        );
        // Confirm the test prose really maps to a unique winner;
        // if a future archetype shadow-scores on data:3498 we
        // need to update the test prose.
        let top_score = matches[0].1;
        let tie_threshold = (top_score as f32 * 0.95).floor() as u32;
        let close_count = matches.iter().filter(|(_, s)| *s >= tie_threshold).count();
        assert_eq!(
            close_count,
            1,
            "test setup expects a unique winner outside the 5%-tie window; \
             got {} candidates within tie ({:?})",
            close_count,
            matches
                .iter()
                .map(|(a, s)| (a.id.clone(), *s))
                .collect::<Vec<_>>()
        );
        assert_eq!(
            matches[0].0.id,
            "variant_calling_germline",
            "expected variant_calling_germline as unique winner; got {} (full list {:?})",
            matches[0].0.id,
            matches
                .iter()
                .map(|(a, s)| (a.id.clone(), *s))
                .collect::<Vec<_>>()
        );
        assert!(
            s.archetype_snapshot.is_some(),
            "with goal populated and unique winner, archetype_snapshot must be Some"
        );
        assert_eq!(
            s.archetype_snapshot.as_ref().unwrap().id,
            "variant_calling_germline",
            "snapshotted archetype id must match the find_match winner"
        );
    }
}

/// When the archetype catalog has multiple candidates
/// within the 5%-tie window, we deliberately do NOT auto-snapshot.
/// Proteomics DDA prose routes to `proteomics_dda` archetype. Both proteomics archetypes share
/// `modality_hint: proteomics` (+2) AND `goal_data: data:2976` /
/// `goal_format: format:3475` (+5 from data + format + class).
/// Without `goal_kind_hint`, both score 8 and the snapshot stays
/// None (5%-tie window). With `goal_kind_hint`, the DDA-specific
/// goal pattern (kind: proteomics_dda) lifts proteomics_dda to
/// score 10 vs proteomics_dia at 8 — gap > 5% — DDA wins
/// uniquely.
#[tokio::test]
async fn append_intake_prose_resolves_proteomics_dda_via_kind_hint() {
    let mut s = crate::session::Session::new(false);
    let res = dispatch_one(
        &Tool::Batchable(BatchableTool::AppendIntakeProse {
            prose: "DDA proteomics with TMT labeling using FragPipe for peptide \
                    identification across human samples"
                .into(),
        }),
        &mut s,
        &ctx(),
    )
    .await;
    assert!(!res.is_error, "{:?}", res);
    if let Some(goal) = s.classification.as_ref().and_then(|c| c.goal.as_ref()) {
        assert_eq!(
            goal.modifiers.get("kind").map(|s| s.as_str()),
            Some("proteomics_dda"),
            "DDA goal pattern should match for DDA prose"
        );
        assert_eq!(
            s.archetype_snapshot.as_ref().map(|a| a.id.as_str()),
            Some("proteomics_dda"),
            "kind hint should disambiguate proteomics_dda; got {:?}",
            s.archetype_snapshot.as_ref().map(|a| a.id.clone())
        );
    }
}

/// Proteomics DIA prose routes to `proteomics_dia` archetype.
/// Symmetric counterpart to the DDA test above.
#[tokio::test]
async fn append_intake_prose_resolves_proteomics_dia_via_kind_hint() {
    let mut s = crate::session::Session::new(false);
    let res = dispatch_one(
        &Tool::Batchable(BatchableTool::AppendIntakeProse {
            prose: "DIA proteomics analysis using DIA-NN for label-free \
                    quantification across human plasma samples"
                .into(),
        }),
        &mut s,
        &ctx(),
    )
    .await;
    assert!(!res.is_error, "{:?}", res);
    if let Some(goal) = s.classification.as_ref().and_then(|c| c.goal.as_ref()) {
        assert_eq!(
            goal.modifiers.get("kind").map(|s| s.as_str()),
            Some("proteomics_dia"),
            "DIA goal pattern should match for DIA prose"
        );
        assert_eq!(
            s.archetype_snapshot.as_ref().map(|a| a.id.as_str()),
            Some("proteomics_dia"),
            "kind hint should disambiguate proteomics_dia; got {:?}",
            s.archetype_snapshot.as_ref().map(|a| a.id.clone())
        );
    }
}

/// Bulk-RNA-seq DE prose disambiguates cleanly because the
/// modality_hint scoring
/// component lifts `bulk_rnaseq_de` (modality_hint=bulk_rnaseq,
/// classifier returns bulk_rnaseq → +2) above `long_read_rnaseq`
/// + `metagenomics_taxonomic` (no modality match → +0). Result:
/// bulk_rnaseq_de = 8, peers = 6, gap > 5%-tie window, snapshot
/// pins cleanly.
#[tokio::test]
async fn append_intake_prose_resolves_bulk_de_tie_via_modality_hint() {
    let mut s = crate::session::Session::new(false);
    let res = dispatch_one(
        &Tool::Batchable(BatchableTool::AppendIntakeProse {
            prose: "differential expression analysis on bulk RNA-seq from human samples".into(),
        }),
        &mut s,
        &ctx(),
    )
    .await;
    assert!(!res.is_error, "{:?}", res);
    if s.classification
        .as_ref()
        .and_then(|c| c.goal.as_ref())
        .is_some()
    {
        // Pre-tie-fix: snapshot was None (3-way tie at score 6).
        // Post-tie-fix: snapshot pins bulk_rnaseq_de cleanly.
        assert_eq!(
            s.archetype_snapshot.as_ref().map(|a| a.id.as_str()),
            Some("bulk_rnaseq_de"),
            "with modality_hint set, bulk_rnaseq_de should win uniquely; got {:?}",
            s.archetype_snapshot.as_ref().map(|a| a.id.clone())
        );
    }
}

/// Multi-modality regression scenario.
///
/// Reproduces the autism/PMS/cross-omics intake from the
/// chat session. Asserts the full pipeline:
///
/// 1. **Classifier** populates `additional_modalities` with the
/// secondary modality (the user's intake hits both proteomics
/// and bulk_rnaseq above the cross-omics threshold AND uses
/// "and" between modality nouns, so M1's two-gate predicate
/// fires).
/// 2. **Snapshot** logic in `append_intake_prose` (M4 wiring) pins
/// the `cross_omics_rnaseq_proteomics` archetype rather than a
/// single-modality archetype.
/// 3. **Composer** path (M3) produces a DAG with BOTH the
/// `rnaseq_*` and `proteomics_*` namespaced branches — the bug
/// that triggered the amendment.
///
/// **Requires `composer_version >= 2`.** The legacy taxonomy build
/// (composer_version=1, today's default) does not consult
/// `archetype_snapshot`, so cross-omics fixing requires the session
/// to have committed to the archetype-fast-path composer at
/// creation time (set via `ECAA_COMPOSER=archetypes`). The test
/// sets `composer_version=2` directly on the session because
/// `Session::new` reads the env var and tests run with it unset.
/// The legacy-taxonomy sunset will flip the default, at which point
/// this manual setter becomes redundant.
#[tokio::test]
async fn cross_omics_autism_pms_intake_emits_both_branches() {
    let mut s = crate::session::Session::new(false);
    s.composer_version = 2;
    let res = dispatch_one(
        &Tool::Batchable(BatchableTool::AppendIntakeProse {
            prose: "I want to analyze all publicly available data comparing the gene \
                    expression and proteomics of healthy subjects vs patients with autism \
                    spectrum disorder vs patients with phelan mcdermid syndrome. RNA-seq \
                    differential expression and mass spec proteomics from postmortem brain \
                    tissue."
                .into(),
        }),
        &mut s,
        &ctx(),
    )
    .await;
    assert!(!res.is_error, "{:?}", res);

    // M1 — classifier surfaces both modalities.
    let clf = s
        .classification
        .as_ref()
        .expect("classification must exist");
    let primary = clf.modality.as_str();
    let secondary_ids: Vec<&str> = clf
        .additional_modalities
        .iter()
        .map(|m| m.modality.as_str())
        .collect();
    let all_ids: std::collections::HashSet<&str> = std::iter::once(primary)
        .chain(secondary_ids.iter().copied())
        .collect();
    assert!(
        all_ids.contains("bulk_rnaseq"),
        "bulk_rnaseq must be primary or in additional, got primary={}, additional={:?}",
        primary,
        secondary_ids
    );
    assert!(
        all_ids.contains("proteomics"),
        "proteomics must be primary or in additional, got primary={}, additional={:?}",
        primary,
        secondary_ids
    );

    // M4 — snapshot pins the cross-omics archetype.
    let snapshot = s
        .archetype_snapshot
        .as_ref()
        .expect("cross-omics intake must pin an archetype snapshot");
    assert_eq!(
        snapshot.id, "cross_omics_rnaseq_proteomics",
        "cross-omics intake must pin the cross-omics archetype, got {}",
        snapshot.id
    );

    // M3 — composer-built DAG carries both branches.
    let dag = s.dag.as_ref().expect("DAG must be built");
    let task_ids: std::collections::HashSet<&str> =
        dag.tasks.keys().map(|id| id.as_str()).collect();
    assert!(
        task_ids.iter().any(|id| id.starts_with("rnaseq_")),
        "DAG must contain RNA-seq branch tasks (rnaseq_*), got {:?}",
        task_ids
    );
    assert!(
        task_ids.iter().any(|id| id.starts_with("proteomics_")),
        "DAG must contain proteomics branch tasks (proteomics_*), got {:?}",
        task_ids
    );
    assert!(
        task_ids.contains("cross_omics_thematic_comparison"),
        "DAG must contain the joint thematic-comparison stage, got {:?}",
        task_ids
    );
}

/// Regression for blinded paper-recreation tri-omics prompts.
///
/// The classifier can correctly surface `bulk_rnaseq + atac_seq +
/// chip_seq` while its single-goal extractor picks a branch goal such
/// as "peak calling". Rebuild must honor the explicit cross-omics
/// modality set and route to the ternary archetype instead of falling
/// back to a generic/single-branch DAG.
#[tokio::test]
async fn tri_omics_branch_goal_routes_to_cross_omics_archetype() {
    let mut s = crate::session::Session::new(false);
    s.composer_version = 4;
    let prose = "We're doing a three-way analysis on a cohort of around twenty \
        patient-matched samples: bulk RNA-seq, bulk ATAC-seq, and ChIP-seq for \
        an activating histone mark, all from the same donors split between a \
        malignant population and a matched healthy hematopoietic population. \
        Raw FASTQs are available for all three modalities. Reference GRCh38. \
        We want each branch run separately first: RNA-seq differential \
        expression, ATAC peak calling and a master peak set, ChIP peak calling, \
        and then a cross-modality concordance report near the TSS. Verify the \
        same donor IDs appear across all three modalities.";

    let res = dispatch_one(
        &Tool::Batchable(BatchableTool::AppendIntakeProse {
            prose: prose.into(),
        }),
        &mut s,
        &ctx(),
    )
    .await;
    assert!(!res.is_error, "{:?}", res);

    let clf = s
        .classification
        .as_ref()
        .expect("classification must exist");
    let all_modalities: std::collections::HashSet<&str> = std::iter::once(clf.modality.as_str())
        .chain(
            clf.additional_modalities
                .iter()
                .map(|m| m.modality.as_str()),
        )
        .collect();
    assert!(
        ["bulk_rnaseq", "atac_seq", "chip_seq"]
            .iter()
            .all(|m| all_modalities.contains(m)),
        "classifier must retain all three omics layers, got primary={} additional={:?}",
        clf.modality,
        clf.additional_modalities
    );
    let workflow_dag = s.workflow_dag.as_ref().expect("workflow DAG must be built");
    let alternatives: Vec<(String, Option<String>, usize, Vec<String>)> = s
        .ranked_alternatives
        .iter()
        .map(|alt| {
            let bad_edges: Vec<String> = alt
                .dag
                .edges
                .iter()
                .filter(|e| {
                    e.proof.producer_type.is_empty()
                        || e.proof.warnings.iter().any(|w| w.contains("incompatible"))
                })
                .take(5)
                .map(|e| {
                    format!(
                        "{}->{} producer_type={:?} warnings={:?}",
                        e.from_node, e.to_node, e.proof.producer_type, e.proof.warnings
                    )
                })
                .collect();
            (
                alt.source.clone(),
                alt.dag.source_template.clone(),
                alt.dag.nodes.len(),
                bad_edges,
            )
        })
        .collect();
    assert_eq!(
        workflow_dag.source_template.as_deref(),
        Some("cross_omics_rnaseq_atac_chip"),
        "primary={} additional={:?} alternatives={alternatives:?}",
        clf.modality,
        clf.additional_modalities
    );
    let task_ids: std::collections::HashSet<&str> =
        workflow_dag.nodes.iter().map(|n| n.id.as_str()).collect();
    for required in [
        "rnaseq_data_acquisition",
        "rnaseq_differential_expression",
        "atac_peak_calling",
        "chip_peak_calling",
        "cross_omics_alignment_check",
        "cross_omics_thematic_comparison",
    ] {
        assert!(
            task_ids.contains(required),
            "missing {required}: {task_ids:?}"
        );
    }
}

/// Exact-shape regression for session
/// `43f93729-8802-4e7d-bf82-bd423cfd8f54` from the web UI.
/// The LLM correctly summarized both omics layers, but the saved
/// session had `classification.modality = proteomics`,
/// `additional_modalities = [bulk_rnaseq]`, `archetype_snapshot = None`,
/// and a legacy proteomics taxonomy DAG. This pins the remediation:
/// once explicit cross-omics intent is captured, rebuild must either
/// emit a cross-omics DAG or fail instead of downgrading to proteomics.
#[tokio::test]
async fn live_session_cross_omics_sequence_does_not_downgrade_to_proteomics() {
    let mut s = crate::session::Session::new(false);
    s.composer_version = 2;
    let turns = [
        "Cross-omics study comparing three groups: healthy controls, autism spectrum disorder \
         (ASD) patients, and Phelan-McDermid syndrome (PMS) patients. Goals: differential \
         gene expression (transcriptomics) and differential protein abundance (proteomics) \
         across the three groups.",
        "Cross-omics study comparing gene expression (transcriptomics) and proteomics across \
         three groups: healthy controls, autism spectrum disorder (ASD) patients, and \
         Phelan-McDermid syndrome (PMS) patients. Sample type: postmortem brain tissue. Data \
         sourcing: comprehensive sweeps of all publicly available repositories (e.g., GEO, \
         ArrayExpress, SRA, PRIDE, MassIVE, ProteomeXchange) to identify any and all \
         available datasets matching these criteria. No private/in-house data — public-only.",
        "Cross-omics analysis comparing three groups: healthy controls, autism spectrum \
         disorder (ASD), and Phelan-McDermid syndrome (PMS). Sample type: postmortem brain \
         tissue. Modalities in scope: Transcriptomics — bulk RNA-seq preferred, but accept \
         the largest available cohort even if microarray or single-cell/single-nucleus. \
         Proteomics — LC-MS/MS, whatever is available across the three groups.",
        "Cross-omics analysis combining bulk RNA-seq transcriptomics and LC-MS/MS proteomics \
         from postmortem human brain tissue. Three groups: healthy controls, autism spectrum \
         disorder (ASD) patients, and Phelan-McDermid syndrome (PMS) patients. Repository \
         sweep across all publicly available sources for both transcriptomics and proteomics \
         data in any available form. Any brain region included but analyses separated by \
         brain region. All three pairwise comparisons plus shared ASD/PMS signal.",
        "Cross-omics analysis of postmortem human brain tissue comparing three groups: healthy \
         controls, autism spectrum disorder (ASD) patients, and Phelan-McDermid syndrome \
         (PMS) patients. Omics layers: 1. Bulk RNA-seq transcriptomics (will accept \
         single-cell/single-nucleus if it is the largest available dataset) 2. LC-MS/MS \
         proteomics (raw files or pre-processed matrices, whatever is available). Shared \
         signal analysis: formal convergence test and overlap/Venn approach.",
    ];

    for prose in turns {
        let res = dispatch_one(
            &Tool::Batchable(BatchableTool::AppendIntakeProse {
                prose: prose.into(),
            }),
            &mut s,
            &ctx(),
        )
        .await;
        assert!(!res.is_error, "{:?}", res);
    }

    let clf = s
        .classification
        .as_ref()
        .expect("classification must exist");
    let all_modalities: std::collections::HashSet<&str> = std::iter::once(clf.modality.as_str())
        .chain(
            clf.additional_modalities
                .iter()
                .map(|m| m.modality.as_str()),
        )
        .collect();
    assert!(
        all_modalities.contains("bulk_rnaseq") && all_modalities.contains("proteomics"),
        "classifier must retain both omics layers, got primary={} additional={:?}",
        clf.modality,
        clf.additional_modalities
    );

    let dag = s.dag.as_ref().expect("DAG must be built");
    let task_ids: std::collections::HashSet<&str> =
        dag.tasks.keys().map(|id| id.as_str()).collect();
    assert!(
        (task_ids.contains("rnaseq_data_acquisition")
            && task_ids.contains("rnaseq_differential_expression"))
            || (task_ids.contains("bulk_rnaseq_data_acquisition")
                && task_ids.contains("bulk_rnaseq_differential_expression")),
        "DAG must include RNA-seq branch tasks, got {:?}",
        task_ids
    );
    // The proteomics_dda archetype names the DE atom
    // `differential_expression` (shared with bulk_rnaseq); when the
    // composer mounts the proteomics_dda branch under the
    // `proteomics_` id_prefix the resulting task id is
    // `proteomics_differential_expression`. Accept either spelling so
    // a future rename to `differential_abundance` won't silently break
    // the test.
    assert!(
        task_ids.contains("proteomics_data_acquisition")
            && (task_ids.contains("proteomics_differential_abundance")
                || task_ids.contains("proteomics_differential_expression")),
        "DAG must include proteomics branch tasks, got {:?}",
        task_ids
    );
    assert!(
        task_ids.contains("cross_omics_thematic_comparison")
            || task_ids.contains("multi_modal_thematic_comparison"),
        "DAG must include cross-omics join task, got {:?}",
        task_ids
    );
}

#[tokio::test]
async fn user_gene_expression_proteomics_text_builds_full_multiomics_dag() {
    let mut s = crate::session::Session::new(false);
    s.composer_version = 2;
    // "cross-omics" is a strong marker that lowers the companion-modality
    // min_hits threshold from 3 → 1; without it, the "bulk_rnaseq+proteomics"
    // suppressed pair would not surface as cross-omics intent (fd927f33).
    let prose = "i need to perform a cross-omics gene expression and proteomics analysis \
        comparing healthy subjects vs patients with autism spectrum disorder vs patients with \
        phelan mcdermid syndrome. we must perform a sweep of all publicly available repositories \
        to identify all available postmortem brain tissue from any region and during the analysis \
        the regions should be analyzed and compared within region. any data where we do not have \
        enough data available to meet statistical power requirements we should drop the group. \
        i need formal convergence between ASD/PMS as well as general overlap and i also need a \
        comparison of what is different between healthy/asd/pms";

    let res = dispatch_one(
        &Tool::Batchable(BatchableTool::AppendIntakeProse {
            prose: prose.into(),
        }),
        &mut s,
        &ctx(),
    )
    .await;
    assert!(!res.is_error, "{:?}", res);

    let clf = s
        .classification
        .as_ref()
        .expect("classification must exist");
    let all_modalities: std::collections::HashSet<&str> = std::iter::once(clf.modality.as_str())
        .chain(
            clf.additional_modalities
                .iter()
                .map(|m| m.modality.as_str()),
        )
        .collect();
    assert!(
        all_modalities.contains("bulk_rnaseq") && all_modalities.contains("proteomics"),
        "classifier must retain gene-expression and proteomics layers, got primary={} additional={:?}",
        clf.modality,
        clf.additional_modalities
    );

    let dag = s.dag.as_ref().expect("multi-omics DAG must be built");
    let task_ids: std::collections::HashSet<&str> =
        dag.tasks.keys().map(|id| id.as_str()).collect();
    assert!(task_ids.contains("rnaseq_data_acquisition"));
    assert!(task_ids.contains("rnaseq_differential_expression"));
    assert!(task_ids.contains("proteomics_data_acquisition"));
    assert!(task_ids.contains("proteomics_differential_abundance"));
    assert!(task_ids.contains("cross_omics_thematic_comparison"));
    assert!(task_ids.contains("final_reporting"));
    assert!(
        !task_ids.contains("differential_expression")
            || task_ids.iter().any(|id| id.starts_with("rnaseq_")),
        "DAG must not be a proteomics-only or generic single-modality graph: {:?}",
        task_ids
    );
}

#[tokio::test]
async fn latest_session_shape_composes_three_branches_then_allows_scope_reset() {
    let mut s = crate::session::Session::new(false);
    s.composer_version = 2;

    let cross_omics = "Cross-omics analysis: human postmortem brain, both transcriptomics \
        (bulk RNA-seq AND single-nucleus/single-cell RNA-seq where available) AND proteomics \
        (LC-MS/MS). Three groups compared: healthy controls vs autism spectrum disorder \
        (ASD) vs Phelan-McDermid syndrome (PMS). Required outputs: differential signal, \
        formal convergence between ASD and PMS, general overlap, and divergence.";
    let res = dispatch_one(
        &Tool::Batchable(BatchableTool::AppendIntakeProse {
            prose: cross_omics.into(),
        }),
        &mut s,
        &ctx(),
    )
    .await;
    assert!(!res.is_error, "{:?}", res);

    let dag = s.dag.as_ref().expect("cross-omics DAG must be built");
    let task_ids: std::collections::HashSet<&str> =
        dag.tasks.keys().map(|id| id.as_str()).collect();
    assert!(task_ids.contains("bulk_rnaseq_differential_expression"));
    assert!(task_ids.contains("single_cell_rnaseq_differential_expression"));
    assert!(task_ids.contains("proteomics_differential_expression"));
    assert!(task_ids.contains("multi_modal_thematic_comparison"));

    let correction = "Bulk RNA-seq transcriptomics analysis only (no proteomics, no \
        single-cell/single-nucleus this session). Human postmortem brain tissue only. \
        Differential expression comparing healthy controls vs ASD vs PMS.";
    let res = dispatch_one(
        &Tool::Batchable(BatchableTool::AppendIntakeProse {
            prose: correction.into(),
        }),
        &mut s,
        &ctx(),
    )
    .await;
    assert!(!res.is_error, "{:?}", res);

    let clf = s
        .classification
        .as_ref()
        .expect("classification must exist");
    assert_eq!(clf.modality, "bulk_rnaseq");
    assert!(
        clf.additional_modalities.is_empty(),
        "scope reset must remove stale cross-omics companions, got {:?}",
        clf.additional_modalities
    );
    let dag = s.dag.as_ref().expect("bulk-only DAG must be rebuilt");
    let task_ids: std::collections::HashSet<&str> =
        dag.tasks.keys().map(|id| id.as_str()).collect();
    assert!(task_ids.contains("differential_expression"));
    assert!(
        !task_ids.iter().any(|id| id.starts_with("proteomics_")),
        "bulk-only correction must not leave proteomics tasks in DAG: {:?}",
        task_ids
    );
}

/// v3 P8 follow-up — `enqueue_adjudication` must emit both a
/// `LifecycleAdversarialEdgeDetected` row and an `AdjudicationEnqueued`
/// row onto the verifier-decision substrate so post-hoc replay can
/// reconstruct lifecycle drama without joining a separate sidecar.
/// Locks the wiring at the call site (the F18 substrate-completeness
/// property test asserts the invariant; this is the call-site
/// regression).
#[test]
fn enqueue_adjudication_emits_substrate_pair() {
    use ecaa_workflow_core::decision_substrate::{drain, VerifierDecision};
    use ecaa_workflow_core::lifecycle_adversarial::LifecycleTransition;

    // Drain so the test reads only its own emissions. The substrate
    // buffer is process-wide; other tests in this binary may have
    // accumulated rows. We filter by `transition_kind` after drain
    // so cross-test pollution doesn't fail the assertion.
    let _ = drain();

    let mut session = crate::session::Session::new(false);
    let transition = LifecycleTransition::SameUserContradiction {
        actor: "sme".into(),
        assumption_id: "a_substrate_pair_test".into(),
        prior_record_id: "rec_1".into(),
        new_record_id: "rec_2".into(),
    };
    super::enqueue_adjudication(&mut session, transition);

    let events = drain();
    let detected: Vec<&VerifierDecision> = events
        .iter()
        .filter(|d| {
            matches!(
                d,
                VerifierDecision::LifecycleAdversarialEdgeDetected { affected_node_id, .. }
                    if affected_node_id == "a_substrate_pair_test"
            )
        })
        .collect();
    let enqueued: Vec<&VerifierDecision> = events
        .iter()
        .filter(|d| {
            matches!(d, VerifierDecision::AdjudicationEnqueued { transition_kind, .. }
            if transition_kind == "same_user_contradiction")
        })
        .collect();

    assert_eq!(
        detected.len(),
        1,
        "expected one LifecycleAdversarialEdgeDetected for the test's transition, got {} events: {:?}",
        events.len(),
        events
    );
    assert_eq!(
        enqueued.len(),
        1,
        "expected one AdjudicationEnqueued for the test's transition, got {} events: {:?}",
        events.len(),
        events
    );
    // The queue entry id format is `adj_<12 hex>`. The substrate's
    // `queue_entry_id` field must reference that id.
    if let VerifierDecision::AdjudicationEnqueued { queue_entry_id, .. } = enqueued[0] {
        assert!(
            queue_entry_id.starts_with("adj_"),
            "queue_entry_id should start with adj_, got: {queue_entry_id}"
        );
        assert_eq!(
            queue_entry_id, &session.adjudication_queue[0].id,
            "substrate queue_entry_id must match the queue entry's id"
        );
    } else {
        unreachable!();
    }

    // Idempotency — calling `enqueue_adjudication` again with the
    // same transition is a no-op for the queue (early return). The
    // substrate is fire-and-forget; a second call should NOT emit
    // duplicate rows because the early-return path is taken before
    // any `record(...)` calls.
    let prior_queue_len = session.adjudication_queue.len();
    let prior_substrate_len = drain().len();
    let _ = drain();
    super::enqueue_adjudication(
        &mut session,
        LifecycleTransition::SameUserContradiction {
            actor: "sme".into(),
            assumption_id: "a_substrate_pair_test".into(),
            prior_record_id: "rec_1".into(),
            new_record_id: "rec_2".into(),
        },
    );
    let second_pass = drain();
    let new_detected = second_pass
        .iter()
        .filter(|d| {
            matches!(
                d,
                VerifierDecision::LifecycleAdversarialEdgeDetected { affected_node_id, .. }
                    if affected_node_id == "a_substrate_pair_test"
            )
        })
        .count();
    assert_eq!(
        session.adjudication_queue.len(),
        prior_queue_len,
        "queue should not grow on duplicate transition"
    );
    assert_eq!(
        new_detected, 0,
        "substrate must not emit duplicate rows on idempotent enqueue (had {} prior substrate events)",
        prior_substrate_len
    );
}

mod state_machine_centralization {
    //! Regression test confirming zero residual
    //! `try_transition(` calls in `crates/conversation/src/tools/<handler>.rs`
    //! files (excluding `mod.rs`, which hosts the centralized
    //! pre/post-handler hook closures). Adding a new mutating tool
    //! that drives a state-machine transition must populate
    //! [`super::ToolSpec::state_trigger`] / [`super::ToolSpec::post_handler`]
    //! and push deferred triggers via [`crate::session::Session::deferred_state_triggers`]
    //! rather than invoking `try_transition` directly.
    //!
    //! `mod.rs` is excluded because the closures co-located with the
    //! `SPEC_*` consts (e.g. `drain_deferred_state_triggers_post_ok`,
    //! `emit_package_post_ok/post_err`) are the centralized firing
    //! site — moving them into per-handler files would defeat the
    //! purpose of the centralization.
    use std::path::Path;

    #[test]
    fn all_handlers_state_clean() {
        let tools_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/tools");
        assert!(
            tools_dir.exists(),
            "tools/ directory missing at {}",
            tools_dir.display()
        );

        let mut violators: Vec<String> = Vec::new();
        for entry in std::fs::read_dir(&tools_dir).expect("read tools dir") {
            let entry = entry.expect("dir entry");
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some("rs") {
                continue;
            }
            let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
            // mod.rs hosts the centralized hook closures; tests.rs
            // exists for fixture seeding (a few callsites do drive
            // state for assertion setup); test_support is the
            // sub-mod equivalent. None of these are dispatch handlers.
            if matches!(stem, "mod" | "tests") {
                continue;
            }
            let body = std::fs::read_to_string(&path).expect("read handler file");
            // The string we forbid is `.try_transition(`. Allow the
            // word in comments by also requiring `session` or
            // `self` to be near it; simplest: check the byte
            // sequence appears anywhere outside of comments. We
            // approximate with a line-level filter that strips
            // `//`-prefixed comments before searching.
            for (lineno, line) in body.lines().enumerate() {
                let code = match line.find("//") {
                    Some(i) => &line[..i],
                    None => line,
                };
                if code.contains("try_transition(") {
                    violators.push(format!("{}:{} — {}", stem, lineno + 1, line.trim()));
                }
            }
        }

        assert!(
            violators.is_empty(),
            "Plan §S16.5 — handler files must not call `try_transition(` \
             directly. Push triggers onto `Session::deferred_state_triggers` \
             or use `ToolSpec::state_trigger` instead. Violations:\n  - {}",
            violators.join("\n  - ")
        );
    }

    /// Regression — D4: chained Promoted proposals where B's upstream
    /// is A (both SME-approved) must produce edges A→B AND B→C in the
    /// re-injected dag, regardless of HashMap iteration order over
    /// `session.proposals`.
    ///
    /// Before the 2-pass split: rebuild_dag iterated `to_inject` once,
    /// pushing each node and immediately walking its `upstream_atom_ids`.
    /// If the leaf (C with upstream B) was visited before its sibling
    /// (B with upstream A), the C→B edge was skipped with a "upstream
    /// not in current DAG" warning, and `WORKFLOW.json` carried
    /// `C.depends_on=[]`. Repro at scen-01 IBD: leave_one_cohort_out
    /// emitted with empty depends_on though its proposal correctly
    /// named random_effects_meta_analysis as upstream.
    #[test]
    fn rebuild_dag_reinject_wires_promoted_to_promoted_chain() {
        use crate::session::Session;
        use ecaa_workflow_core::hypothesized_proposal::{
            HypothesizedProposal, ProposalLifecycle,
        };
        use ecaa_workflow_core::workflow_contracts::task_node::{TaskNode, WorkflowDag};

        let mut s = Session::new(false);

        // Seed the dag with a single catalog atom that BOTH promoted
        // proposals can claim transitively as their root upstream.
        s.workflow_dag = Some(WorkflowDag {
            id: "wf-d4".into(),
            nodes: vec![TaskNode::skeleton(
                "differential_expression",
                "stable catalog atom",
            )],
            edges: vec![],
            assumptions: Default::default(),
            source_template: None,
        });

        // PARENT proposal (B): random_effects_meta_analysis, upstream = differential_expression
        let mut parent = HypothesizedProposal::new(
            /* node_id          */ "random_effects_meta_analysis",
            /* intent           */ "Random-effects meta-analysis over per-cohort DE",
            /* parent_terms     */ vec!["data:0951".into()],
            /* llm_rationale    */ "SME requested random-effects meta",
            /* assumptions      */ vec![],
            /* failure_modes    */ vec![],
            /* validation_tests */ vec![],
            /* upstream_atom_ids*/ vec!["differential_expression".into()],
        );
        parent.lifecycle = ProposalLifecycle::Promoted {
            task_node_id: "random_effects_meta_analysis".into(),
        };

        // LEAF proposal (C): leave_one_cohort_out_sensitivity, upstream = B (random_effects_meta_analysis)
        let mut leaf = HypothesizedProposal::new(
            /* node_id          */ "leave_one_cohort_out_sensitivity",
            /* intent           */ "Leave-one-cohort-out sensitivity on meta-analysis",
            /* parent_terms     */ vec!["data:0951".into()],
            /* llm_rationale    */ "SME requested LOO sensitivity",
            /* assumptions      */ vec![],
            /* failure_modes    */ vec![],
            /* validation_tests */ vec![],
            /* upstream_atom_ids*/ vec!["random_effects_meta_analysis".into()],
        );
        leaf.lifecycle = ProposalLifecycle::Promoted {
            task_node_id: "leave_one_cohort_out_sensitivity".into(),
        };

        s.proposals.insert(parent.id.clone(), parent);
        s.proposals.insert(leaf.id.clone(), leaf);

        // Drive the re-injection helper directly. Extracting it from
        // rebuild_dag lets the unit test exercise the 2-pass ordering
        // without needing a real taxonomy/classification on the session.
        crate::tools::reinject_promoted_nodes_into_workflow_dag(&mut s);

        let dag = s
            .workflow_dag
            .as_ref()
            .expect("workflow_dag must be present after rebuild_dag");
        let node_ids: Vec<&String> = dag.nodes.iter().map(|n| &n.id).collect();
        assert!(
            node_ids
                .iter()
                .any(|i| *i == "random_effects_meta_analysis"),
            "parent promoted node must be re-injected; nodes: {:?}",
            node_ids
        );
        assert!(
            node_ids
                .iter()
                .any(|i| *i == "leave_one_cohort_out_sensitivity"),
            "leaf promoted node must be re-injected; nodes: {:?}",
            node_ids
        );

        let has_edge = |from: &str, to: &str| {
            dag.edges
                .iter()
                .any(|e| e.from_node == from && e.to_node == to)
        };
        assert!(
            has_edge("differential_expression", "random_effects_meta_analysis"),
            "catalog→parent edge must be wired; edges: {:?}",
            dag.edges
                .iter()
                .map(|e| format!("{}→{}", e.from_node, e.to_node))
                .collect::<Vec<_>>()
        );
        assert!(
            has_edge(
                "random_effects_meta_analysis",
                "leave_one_cohort_out_sensitivity"
            ),
            "parent→leaf edge (the D4 bug) must be wired; edges: {:?}",
            dag.edges
                .iter()
                .map(|e| format!("{}→{}", e.from_node, e.to_node))
                .collect::<Vec<_>>()
        );
    }

    /// When the novel-method override in `try_build_via_composer`
    /// routes a session to the `generic_omics` archetype (CyTOF,
    /// Mendelian randomization, Slide-seq, Cryo-EM, snmC-seq, CODEX,
    /// cox regression, strain-level metagenomics), the resulting DAG
    /// carries `generic_summary` as its terminal — not `reporting` or
    /// `final_reporting`. The downstream wiring loop in
    /// `reinject_promoted_nodes_into_workflow_dag` must fall back to
    /// `generic_summary` so promoted analytical atoms connect to the
    /// SME's report instead of becoming strands with only their
    /// `validate_*` companion downstream.
    #[test]
    fn rebuild_dag_reinject_wires_promoted_to_generic_summary() {
        use crate::session::Session;
        use ecaa_workflow_core::hypothesized_proposal::{
            HypothesizedProposal, ProposalLifecycle,
        };
        use ecaa_workflow_core::workflow_contracts::task_node::{TaskNode, WorkflowDag};

        let mut s = Session::new(false);

        // Seed the dag with the generic_omics scaffold:
        //   data_acquisition → raw_qc → generic_summary
        // No `reporting` or `final_reporting` node — only `generic_summary`,
        // which is the terminal shape the novel-method override produces.
        s.workflow_dag = Some(WorkflowDag {
            id: "wf-generic".into(),
            nodes: vec![
                TaskNode::skeleton("data_acquisition", "scaffold root"),
                TaskNode::skeleton("raw_qc", "scaffold qc"),
                TaskNode::skeleton("generic_summary", "scaffold terminal"),
            ],
            edges: vec![],
            assumptions: Default::default(),
            source_template: None,
        });

        // Promoted CyTOF-like atom whose upstream is data_acquisition.
        let mut promoted = HypothesizedProposal::new(
            "cytof_differential_abundance",
            "Differential cluster abundance via edgeR",
            vec!["data:2531".into()],
            "SME requested differential abundance",
            vec![],
            vec![],
            vec![],
            vec!["data_acquisition".into()],
        );
        promoted.lifecycle = ProposalLifecycle::Promoted {
            task_node_id: "cytof_differential_abundance".into(),
        };
        s.proposals.insert(promoted.id.clone(), promoted);

        crate::tools::reinject_promoted_nodes_into_workflow_dag(&mut s);

        let dag = s
            .workflow_dag
            .as_ref()
            .expect("workflow_dag must be present after reinject");
        let has_edge = |from: &str, to: &str| {
            dag.edges
                .iter()
                .any(|e| e.from_node == from && e.to_node == to)
        };
        assert!(
            has_edge("cytof_differential_abundance", "generic_summary"),
            "promoted atom must wire to generic_summary when neither reporting \
             nor final_reporting exists; edges: {:?}",
            dag.edges
                .iter()
                .map(|e| format!("{}→{}", e.from_node, e.to_node))
                .collect::<Vec<_>>()
        );
    }

    /// When the SME signs off a `propose_hypothesized_node` during
    /// `PendingConfirmation`, the signoff handler splices the
    /// materialized node into `session.workflow_dag`. The subsequent
    /// `rebuild_dag_after_signoff → rebuild_dag` sequence then (a)
    /// repopulates `session.dag` from the composer's COMPOSER-ONLY
    /// output (no promoted nodes — the composer builds from
    /// classification, not from the existing workflow_dag), (b) calls
    /// `reinject_promoted_nodes_into_workflow_dag` which mutates only
    /// `session.workflow_dag`. Without the lowered-cache re-derive in
    /// `rebuild_dag`, `emit_steps` would read the stale `session.dag`
    /// and the emitted WORKFLOW.json would omit the promoted node.
    ///
    /// The fix: `reinject_promoted_nodes_into_workflow_dag` tracks whether
    /// the workflow_dag was actually mutated; if yes, re-lowers
    /// `session.dag` from the freshly-updated `session.workflow_dag` so
    /// the emit-time read of `session.dag` includes promoted proposals.
    ///
    /// This test simulates the exact failure mode: pre-fill `session.dag`
    /// with a composer-shape cache (NO promoted node), insert a Promoted
    /// proposal whose materialized id is not in `session.dag.tasks`, run
    /// reinject, and assert `session.dag.tasks` now contains the promoted
    /// task id.
    #[test]
    fn reinject_promoted_refreshes_lowered_dag_cache() {
        use crate::session::Session;
        use ecaa_workflow_core::hypothesized_proposal::{
            HypothesizedProposal, ProposalLifecycle,
        };
        use ecaa_workflow_core::workflow_contracts::task_node::{TaskNode, WorkflowDag};

        let mut s = Session::new(false);

        // Seed `session.workflow_dag` with two catalog atoms — composer
        // pre-signoff output: a `qc_preprocessing` source and a
        // `reporting` terminal. The signoff handler splices the promoted
        // `cell_cell_communication` into the workflow_dag as its
        // in-closure mutation; we simulate that pre-condition by pushing
        // the node here so reinject sees the "post-rebuild + reinjected"
        // state in workflow_dag and exercises the cache-rederive code
        // path against a fresh-from-composer `session.dag`.
        let promoted_node =
            TaskNode::skeleton("cell_cell_communication", "Cell-cell communication scoring");
        s.workflow_dag = Some(WorkflowDag {
            id: "wf-defect-2026-05-19".into(),
            nodes: vec![
                TaskNode::skeleton("qc_preprocessing", "QC + preprocessing"),
                TaskNode::skeleton("reporting", "Final reporting"),
                promoted_node,
            ],
            edges: vec![],
            assumptions: Default::default(),
            source_template: None,
        });

        // Pre-fill `session.dag` (the lowered cache) with the
        // COMPOSER-ONLY shape — `qc_preprocessing` and `reporting` but
        // NOT `cell_cell_communication`. This is exactly the state
        // `rebuild_dag` leaves the session in after running the composer
        // and BEFORE the reinject pass: `session.dag` has the composer
        // output, `session.workflow_dag` has the spliced + reinjected
        // node, and the two are out of sync. The fix re-lowers
        // `session.dag` from `session.workflow_dag` so emit reads a
        // fresh cache.
        //
        // We lower from a stub WorkflowDag that omits the promoted node
        // so the cache content models "composer output without
        // promoted" exactly.
        let stub_wf = WorkflowDag {
            id: "wf-defect-2026-05-19".into(),
            nodes: vec![
                TaskNode::skeleton("qc_preprocessing", "QC + preprocessing"),
                TaskNode::skeleton("reporting", "Final reporting"),
            ],
            edges: vec![],
            assumptions: Default::default(),
            source_template: None,
        };
        let stale_cache = ecaa_workflow_core::builder::build_dag_from_workflow_dag(
            &stub_wf,
            "wf-defect-2026-05-19",
        )
        .expect("stub composer-only DAG lowers cleanly");
        s.dag = Some(stale_cache);

        // Seed the Promoted proposal that the SME signed off.
        let mut proposal = HypothesizedProposal::new(
            "cell_cell_communication",
            "Score ligand-receptor pairs per cell-type pair",
            vec!["data:2603".into()],
            "SME asked for cell-cell comms scoring",
            vec![],
            vec![],
            vec![],
            vec!["qc_preprocessing".into()],
        );
        proposal.lifecycle = ProposalLifecycle::Promoted {
            task_node_id: "cell_cell_communication".into(),
        };
        s.proposals.insert(proposal.id.clone(), proposal);

        // Sanity check: pre-reinject the stale cache LACKS the promoted
        // task — this is the exact emit-time failure mode the fix
        // addresses.
        assert!(
            !s.dag
                .as_ref()
                .map(|d| d.tasks.contains_key("cell_cell_communication"))
                .unwrap_or(false),
            "setup: pre-reinject session.dag (lowered cache) must NOT contain the \
             promoted task — that's the defect we are reproducing"
        );

        // Run reinject. The fix re-lowers session.dag from the
        // freshly-updated session.workflow_dag (which already contains
        // the promoted node), so the cache must now include
        // cell_cell_communication.
        crate::tools::reinject_promoted_nodes_into_workflow_dag(&mut s);

        let dag = s
            .dag
            .as_ref()
            .expect("session.dag must remain Some after reinject");
        assert!(
            dag.tasks.contains_key("cell_cell_communication"),
            "post-reinject session.dag must include the promoted task; \
             present task ids: {:?}",
            dag.tasks.keys().collect::<Vec<_>>()
        );
        // The validator wrapper synthesized by reinject must also flow
        // through to the lowered cache so `validate_<id>` companion
        // tasks reach WORKFLOW.json alongside the promoted task.
        assert!(
            dag.tasks.contains_key("validate_cell_cell_communication"),
            "post-reinject session.dag must include the synthesized validator \
             wrapper; present task ids: {:?}",
            dag.tasks.keys().collect::<Vec<_>>()
        );
    }

    /// Regression — intake-fact post-filters must prune the
    /// authoritative typed `WorkflowDag`, not only the derived
    /// `session.dag` cache. Otherwise a later `ensure_dag_cached()`
    /// call (notably emit_package) re-lowers from `workflow_dag` and
    /// resurrects raw-read / literature tasks that the UI DAG had
    /// already gated out.
    #[test]
    fn intake_fact_gate_prunes_authoritative_workflow_dag_and_cache() {
        use crate::session::Session;
        use ecaa_workflow_core::workflow_contracts::edge::{CompatibilityProof, EdgeContract};
        use ecaa_workflow_core::workflow_contracts::task_node::{TaskNode, WorkflowDag};

        fn edge(from: &str, to: &str) -> EdgeContract {
            EdgeContract {
                from_node: from.into(),
                from_port: "out".into(),
                to_node: to.into(),
                to_port: "in".into(),
                proof: CompatibilityProof::default(),
                chain_of_custody: None,
            }
        }

        let mut s = Session::new(false);
        s.workflow_dag = Some(WorkflowDag {
            id: "wf-intake-gates".into(),
            nodes: vec![
                TaskNode::skeleton("data_acquisition", "input"),
                TaskNode::skeleton("raw_qc", "raw reads qc"),
                TaskNode::skeleton("validate_raw_qc", "validate raw qc"),
                TaskNode::skeleton("discover_raw_qc", "discover raw qc"),
                TaskNode::skeleton("qc_preprocessing", "qc preprocessing"),
                TaskNode::skeleton("review_prior_work", "literature review"),
                TaskNode::skeleton("validate_review_prior_work", "validate lit review"),
                TaskNode::skeleton(
                    "contextualize_findings_with_literature",
                    "literature context",
                ),
                TaskNode::skeleton(
                    "validate_contextualize_findings_with_literature",
                    "validate lit context",
                ),
                TaskNode::skeleton("reporting", "report"),
            ],
            edges: vec![
                edge("data_acquisition", "raw_qc"),
                edge("raw_qc", "validate_raw_qc"),
                edge("raw_qc", "qc_preprocessing"),
                edge("data_acquisition", "discover_raw_qc"),
                edge(
                    "review_prior_work",
                    "contextualize_findings_with_literature",
                ),
                edge(
                    "contextualize_findings_with_literature",
                    "validate_contextualize_findings_with_literature",
                ),
                edge("contextualize_findings_with_literature", "reporting"),
                edge("qc_preprocessing", "reporting"),
            ],
            assumptions: Default::default(),
            source_template: None,
        });

        {
            let mut guard = s.workflow_dag_mut();
            let wf = guard.as_mut().expect("workflow dag seeded");
            let dropped = crate::tools::prune_workflow_dag_roots_with_companions(
                wf,
                &[
                    "raw_qc",
                    "review_prior_work",
                    "contextualize_findings_with_literature",
                ],
            );
            assert!(dropped.contains("raw_qc"));
            assert!(dropped.contains("validate_raw_qc"));
            assert!(dropped.contains("discover_raw_qc"));
            assert!(dropped.contains("review_prior_work"));
            assert!(dropped.contains("validate_review_prior_work"));
            assert!(dropped.contains("contextualize_findings_with_literature"));
            assert!(dropped.contains("validate_contextualize_findings_with_literature"));
        }

        let wf = s
            .workflow_dag
            .as_ref()
            .expect("workflow dag survives prune");
        let node_ids: std::collections::BTreeSet<&str> =
            wf.nodes.iter().map(|n| n.id.as_str()).collect();
        for dropped in [
            "raw_qc",
            "validate_raw_qc",
            "discover_raw_qc",
            "review_prior_work",
            "validate_review_prior_work",
            "contextualize_findings_with_literature",
            "validate_contextualize_findings_with_literature",
        ] {
            assert!(
                !node_ids.contains(dropped),
                "authoritative workflow_dag must not retain gated node {dropped}"
            );
            assert!(
                !wf.edges
                    .iter()
                    .any(|e| e.from_node == dropped || e.to_node == dropped),
                "authoritative workflow_dag must not retain edges touching gated node {dropped}"
            );
        }

        let rebuilt = s
            .ensure_dag_cached()
            .expect("pruned workflow dag must lower cleanly");
        for dropped in [
            "raw_qc",
            "validate_raw_qc",
            "discover_raw_qc",
            "review_prior_work",
            "validate_review_prior_work",
            "contextualize_findings_with_literature",
            "validate_contextualize_findings_with_literature",
        ] {
            assert!(
                !rebuilt.tasks.contains_key(dropped),
                "cache rebuilt from workflow_dag must not resurrect gated node {dropped}"
            );
        }
    }

    /// Regression — chain-middle drops must splice surviving
    /// (parent → child) edges so downstream pipeline atoms keep a
    /// data source. Without the splice, `qc_preprocessing` ends up
    /// with `depends_on: []` after the counts-only-input gate strips
    /// `[raw_qc, sequence_trimming, alignment, quantification]`, and
    /// the whole analytical chain runs as an island.
    #[test]
    fn workflow_dag_prune_splices_chain_middle_drops() {
        use crate::session::Session;
        use ecaa_workflow_core::workflow_contracts::edge::{CompatibilityProof, EdgeContract};
        use ecaa_workflow_core::workflow_contracts::task_node::{TaskNode, WorkflowDag};

        fn edge(from: &str, to: &str) -> EdgeContract {
            EdgeContract {
                from_node: from.into(),
                from_port: "out".into(),
                to_node: to.into(),
                to_port: "in".into(),
                proof: CompatibilityProof::default(),
                chain_of_custody: None,
            }
        }

        let mut s = Session::new(false);
        s.workflow_dag = Some(WorkflowDag {
            id: "wf-chain-splice".into(),
            nodes: vec![
                TaskNode::skeleton("data_acquisition", "input"),
                TaskNode::skeleton("raw_qc", "raw qc"),
                TaskNode::skeleton("validate_raw_qc", "validate raw qc"),
                TaskNode::skeleton("sequence_trimming", "trim"),
                TaskNode::skeleton("validate_sequence_trimming", "validate trim"),
                TaskNode::skeleton("alignment", "align"),
                TaskNode::skeleton("validate_alignment", "validate align"),
                TaskNode::skeleton("quantification", "quant"),
                TaskNode::skeleton("validate_quantification", "validate quant"),
                TaskNode::skeleton("qc_preprocessing", "count qc"),
                TaskNode::skeleton("normalisation", "norm"),
            ],
            edges: vec![
                edge("data_acquisition", "raw_qc"),
                edge("raw_qc", "validate_raw_qc"),
                edge("raw_qc", "sequence_trimming"),
                edge("sequence_trimming", "validate_sequence_trimming"),
                edge("sequence_trimming", "alignment"),
                edge("alignment", "validate_alignment"),
                edge("alignment", "quantification"),
                edge("quantification", "validate_quantification"),
                edge("quantification", "qc_preprocessing"),
                edge("qc_preprocessing", "normalisation"),
            ],
            assumptions: Default::default(),
            source_template: None,
        });

        {
            let mut guard = s.workflow_dag_mut();
            let wf = guard.as_mut().expect("workflow dag seeded");
            let _ = crate::tools::prune_workflow_dag_roots_with_companions(
                wf,
                &["raw_qc", "sequence_trimming", "alignment", "quantification"],
            );
        }

        let wf = s
            .workflow_dag
            .as_ref()
            .expect("workflow dag survives prune");
        let bridge = wf
            .edges
            .iter()
            .find(|e| e.from_node == "data_acquisition" && e.to_node == "qc_preprocessing");
        assert!(
            bridge.is_some(),
            "splice must bridge data_acquisition → qc_preprocessing after FASTQ chain drop; \
             remaining edges: {:?}",
            wf.edges
                .iter()
                .map(|e| (e.from_node.as_str(), e.to_node.as_str()))
                .collect::<Vec<_>>(),
        );
        // No edge may target a validator that no longer exists in the
        // node set, and no validator may have been promoted to a
        // data-source parent.
        for e in &wf.edges {
            assert!(
                !e.from_node.starts_with("validate_"),
                "splice must not promote a validator to a data-source parent: {:?}",
                e
            );
        }
    }
}

// ─────────────────────────────────────────────────────────────────────
// D11: propose_summary_confirmation must observe fresh proposal
// lifecycle (post-/signoff Promotion) when a store is wired into
// ToolContext, instead of trusting the tool-loop's stale local
// snapshot. Mirrors the existing pattern in `tools/emit.rs::emit_package`
// where the store re-read closes the gate-skew race.
// ─────────────────────────────────────────────────────────────────────
#[cfg(test)]
mod d11_proposal_signoff_freshness {
    use super::*;
    use crate::persistence::SessionStore;
    use crate::session::Session;
    use ecaa_workflow_core::hypothesized_proposal::{HypothesizedProposal, ProposalLifecycle};

    fn seed_proposal(
        session: &mut Session,
        node_id: &str,
        lifecycle: ProposalLifecycle,
    ) -> ecaa_workflow_core::hypothesized_proposal::ProposalId {
        let mut p = HypothesizedProposal::new(
            node_id,
            "intent",
            vec!["data:2603".into()],
            "rationale",
            vec![],
            vec![],
            vec![],
            vec![],
        );
        p.lifecycle = lifecycle;
        let id = p.id.clone();
        session.proposals.insert(p.id.clone(), p);
        id
    }

    #[tokio::test]
    async fn propose_summary_confirmation_reads_post_signoff_lifecycle_from_store() {
        // D11: simulate the race the parent session bc17e31f-... hit:
        //   1. tool loop snapshots session at turn-start (proposal
        //      AwaitingSignoff)
        //   2. SME clicks Approve → server's /signoff POST writes
        //      Promoted to the SessionStore
        //   3. LLM-mediated turn dispatch reaches
        //      propose_summary_confirmation, which on the stale local
        //      snapshot returns PreconditionFailure("await SME action")
        //
        // After the fix the precondition consults the persisted store
        // (when wired into ToolContext) and observes Promoted.
        let dir = tempfile::tempdir().expect("tempdir");
        let store = SessionStore::open(dir.path()).await.expect("open store");

        // Persist a session in which the proposal is Promoted (the
        // signoff handler's post-state).
        let mut persisted = Session::new(false);
        let proposal_id = seed_proposal(
            &mut persisted,
            "guide_assignment",
            ProposalLifecycle::Promoted {
                task_node_id: "guide_assignment".into(),
            },
        );
        store.save(&persisted).await.expect("save persisted");

        // Build the stale local snapshot — same proposal id, but the
        // lifecycle still reads AwaitingSignoff (the pre-/signoff
        // state the in-flight tool loop captured at turn-start).
        let mut local = Session {
            id: persisted.id,
            ..Session::new(false)
        };
        let mut stale_proposal = persisted.proposals[&proposal_id].clone();
        stale_proposal.lifecycle = ProposalLifecycle::AwaitingSignoff;
        local.proposals.insert(proposal_id.clone(), stale_proposal);

        // Wire the store into the ToolContext exactly as the production
        // tool_loop does via `.with_store(self.store_handle())`.
        let ctx = ToolContext::new(config_dir(), "claude-sonnet-4-6").with_store(store.clone());

        let res = dispatch_one(
            &Tool::Batchable(BatchableTool::ProposeSummaryConfirmation {
                summary_markdown: "Here is the plan…".into(),
            }),
            &mut local,
            &ctx,
        )
        .await;

        assert!(
            !res.is_error,
            "propose_summary_confirmation must consult the persisted \
             session's proposals, observe Promoted, and accept — got \
             error: {:?}",
            res.content
        );

        // The local snapshot's proposal lifecycle was refreshed
        // in-place so downstream code in the same tool loop (e.g. the
        // emit_package gate that already does its own store re-read)
        // observes the same Promoted state without a second round
        // trip.
        assert!(
            matches!(
                local.proposals[&proposal_id].lifecycle,
                ProposalLifecycle::Promoted { .. }
            ),
            "fresh-read must refresh the local proposal lifecycle in \
             place; got {:?}",
            local.proposals[&proposal_id].lifecycle,
        );
    }

    #[tokio::test]
    async fn fresh_read_preserves_locally_minted_proposals_not_yet_persisted() {
        // Guard against a regression where the fresh-read would
        // wholesale-replace `session.proposals` with the persisted set,
        // silently dropping any proposal the tool loop just created via
        // `propose_hypothesized_node` earlier in the same turn (which
        // hasn't been flushed to the store yet — the persist step lives
        // in send_turn's post-loop merge).
        let dir = tempfile::tempdir().expect("tempdir");
        let store = SessionStore::open(dir.path()).await.expect("open store");

        // Persist a session with one already-Promoted proposal.
        let mut persisted = Session::new(false);
        let old_id = seed_proposal(
            &mut persisted,
            "guide_assignment",
            ProposalLifecycle::Promoted {
                task_node_id: "guide_assignment".into(),
            },
        );
        store.save(&persisted).await.expect("save");

        // Local snapshot: same Promoted proposal PLUS a brand-new
        // proposal that the current turn's tool loop just created
        // (not yet visible to the store).
        let mut local = Session {
            id: persisted.id,
            ..Session::new(false)
        };
        local
            .proposals
            .insert(old_id.clone(), persisted.proposals[&old_id].clone());
        let new_id = seed_proposal(
            &mut local,
            "novel_atom_added_this_turn",
            ProposalLifecycle::AwaitingSignoff,
        );

        let ctx = ToolContext::new(config_dir(), "claude-sonnet-4-6").with_store(store.clone());

        let res = dispatch_one(
            &Tool::Batchable(BatchableTool::ProposeSummaryConfirmation {
                summary_markdown: "plan".into(),
            }),
            &mut local,
            &ctx,
        )
        .await;

        // The newly-minted (this turn) AwaitingSignoff proposal must
        // still block the precondition — the SME has to approve it
        // before the confirmation card raises.
        assert!(
            res.is_error,
            "newly-minted AwaitingSignoff proposal must still block \
             propose_summary_confirmation; got success: {:?}",
            res.content
        );
        let body = serde_json::to_string(&res.content).unwrap();
        assert!(
            body.contains("novel_atom_added_this_turn"),
            "error must surface the locally-minted proposal: {body}"
        );

        // The locally-minted proposal must NOT be dropped by the
        // fresh-read — it stays in session.proposals so the post-loop
        // merge in send_turn persists it forward.
        assert!(
            local.proposals.contains_key(&new_id),
            "fresh-read must not drop locally-minted proposals"
        );
    }
}

/// `try_build_via_composer` must back-fill
/// `session.classification.archetype_id` from `composition.matched_archetype`
/// so that the emitted RO-Crate carries a non-null `matchedArchetype`.
///
/// Prior to this fix, `set_intake_modality` cleared `archetype_id` to
/// `None` (correct — the prior modality's archetype is stale) but
/// `rebuild_dag` never re-populated it after the v4 planner ran.
/// Every emitted package therefore carried `matchedArchetype: null`
/// regardless of which archetype the planner selected.
#[tokio::test]
async fn rebuild_dag_populates_archetype_id_from_matched_archetype() {
    let mut s = crate::session::Session::new(false);

    // Drive the session through intake so the v4 composer runs and
    // picks an archetype. `AppendIntakeProse` triggers `rebuild_dag`
    // via `try_build_via_composer`.
    let res = dispatch_one(
        &Tool::Batchable(BatchableTool::AppendIntakeProse {
            prose: "single cell scRNA-seq differential expression from human IVD samples".into(),
        }),
        &mut s,
        &ctx(),
    )
    .await;
    assert!(
        !res.is_error,
        "append_intake_prose must succeed: {:?}",
        res.content
    );
    assert!(
        s.dag.is_some(),
        "session.dag must be populated after rebuild_dag"
    );

    // The v4 planner must have populated archetype_id on the classification
    // so ro_crate::p_plan_entity can emit a non-null matchedArchetype.
    let archetype_id = s
        .classification
        .as_ref()
        .and_then(|c| c.archetype_id.as_deref());
    assert!(
        archetype_id.is_some(),
        "session.classification.archetype_id must be non-None after rebuild_dag runs \
         the v4 planner. Without this, every emitted RO-Crate carries \
         matchedArchetype: null, breaking ECAA D7 invariants. \
         classification: {:?}",
        s.classification
    );
    // The scRNA-seq intake should match the single_cell_de archetype.
    let id = archetype_id.unwrap();
    assert!(
        id.contains("single_cell") || id.contains("cell_de") || id.contains("scrna"),
        "archetype_id '{}' should be a single-cell archetype for scRNA-seq intake",
        id
    );
}

/// Regression: counts-level entry detected from the exclusion set.
///
/// When the SME declares a counts-only analysis in prose and the LLM
/// excludes the read→counts bridge via `set_intake_excluded_atoms`
/// (`quantification` / `alignment`) WITHOUT registering any input file
/// through the Inputs tab, `session.inputs` is empty so the old
/// `counts_only_inputs` gate returned false and the FASTQ-level atoms
/// (`raw_qc`, `sequence_trimming`) survived — later getting absorbed into
/// `reporting.depends_on` by the lowering pass's orphan-strand repair,
/// producing a semantically false "reporting consumes raw-read QC"
/// dependency. The `counts_level_entry` predicate now also fires on the
/// bridge exclusion, so the whole FASTQ block is pruned.
#[test]
fn counts_level_entry_from_exclusion_prunes_fastq_block() {
    use ecaa_workflow_core::workflow_contracts::edge::{CompatibilityProof, EdgeContract};
    use ecaa_workflow_core::workflow_contracts::evidence::AssumptionLedger;
    use ecaa_workflow_core::workflow_contracts::port::PortContract;
    use ecaa_workflow_core::workflow_contracts::task_node::{TaskNode, WorkflowDag};

    fn node(id: &str) -> TaskNode {
        let mut n = TaskNode::skeleton(id, format!("intent {id}"));
        n.outputs = vec![PortContract::from_edam("out", Some("data:0006"), Some("format:1915"))];
        n.inputs = vec![PortContract::from_edam("in", Some("data:0006"), Some("format:1915"))];
        n
    }
    fn edge(from: &str, to: &str) -> EdgeContract {
        EdgeContract {
            from_node: from.into(),
            from_port: "out".into(),
            to_node: to.into(),
            to_port: "in".into(),
            proof: CompatibilityProof::default(),
            chain_of_custody: None,
        }
    }

    let nodes = [
        "data_acquisition",
        "raw_qc",
        "sequence_trimming",
        "alignment",
        "quantification",
        "qc_preprocessing",
        "normalisation",
        "differential_expression",
        "reporting",
        "validate_raw_qc",
    ]
    .iter()
    .map(|i| node(i))
    .collect::<Vec<_>>();
    let mut wf = WorkflowDag {
        id: "t".into(),
        nodes,
        edges: vec![
            edge("data_acquisition", "raw_qc"),
            edge("raw_qc", "sequence_trimming"),
            edge("sequence_trimming", "alignment"),
            edge("alignment", "quantification"),
            edge("quantification", "qc_preprocessing"),
            edge("qc_preprocessing", "normalisation"),
            edge("normalisation", "differential_expression"),
            edge("differential_expression", "reporting"),
            edge("raw_qc", "validate_raw_qc"),
        ],
        assumptions: AssumptionLedger::default(),
        source_template: None,
    };

    // Empty registered inputs (no Inputs-tab registration) — the old gate
    // would NOT fire here. The counts-level signal comes from the exclusion.
    let mut s = crate::session::Session::new(false);
    s.excluded_atoms = vec!["sequence_trimming".into(), "alignment".into(), "quantification".into()];
    assert!(s.inputs.is_empty(), "test models the no-registered-input path");

    let dropped = super::prune_counts_only_input_workflow_dag(&mut wf, &s);

    let ids: std::collections::BTreeSet<&str> = wf.nodes.iter().map(|n| n.id.as_str()).collect();
    for x in ["raw_qc", "sequence_trimming", "alignment", "quantification", "validate_raw_qc"] {
        assert!(!ids.contains(x), "{x} must be pruned at counts-level entry; got {ids:?}");
    }
    assert!(dropped.contains("raw_qc"), "raw_qc must be reported in the dropped set");
    for keep in ["data_acquisition", "qc_preprocessing", "normalisation", "differential_expression", "reporting"] {
        assert!(ids.contains(keep), "{keep} must survive");
    }
    // The chain drop must splice data_acquisition → qc_preprocessing so the
    // surviving downstream isn't left with an empty depends_on.
    assert!(
        wf.edges.iter().any(|e| super::edge_node_id(&e.from_node) == "data_acquisition"
            && super::edge_node_id(&e.to_node) == "qc_preprocessing"),
        "expected spliced data_acquisition -> qc_preprocessing edge; edges={:?}",
        wf.edges.iter().map(|e| (e.from_node.as_str(), e.to_node.as_str())).collect::<Vec<_>>()
    );
    assert!(
        !wf.edges.iter().any(|e| super::edge_node_id(&e.from_node) == "raw_qc"
            || super::edge_node_id(&e.to_node) == "raw_qc"),
        "no edge may reference pruned raw_qc"
    );
}
