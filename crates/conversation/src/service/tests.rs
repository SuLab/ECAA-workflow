//! Test corpus for `crates/conversation/src/service/mod.rs`.
//!
//! Moved out of `service/mod.rs` into a sibling `tests.rs`
//! file so the prod-only mod.rs stays under the §S5.9 500-LOC cap. The
//! `super::*` glob still resolves to the `service` module (this file
//! is a child of `service` via `#[cfg(test)] mod tests;`).

// File is included only under `#[cfg(test)] mod tests;` in the parent
// (service/mod.rs:219). A sibling `#![cfg(test)]` here is duplicated.

use super::*;
use crate::anthropic::{StopReason, Usage};
use crate::mock::MockLlmBackend;
use crate::session::{SessionState, TurnRole};
use crate::tools::{BatchableTool, Tool};
use uuid::Uuid;

fn config_dir() -> PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("config")
}

fn assistant(text: &str) -> crate::anthropic::TurnResponse {
    crate::anthropic::TurnResponse {
        assistant_content: text.into(),
        tool_uses: vec![],
        stop_reason: StopReason::EndTurn,
        usage: Usage::default(),
        request_metadata: Default::default(),
    }
}

fn assistant_with_usage(text: &str, usage: Usage) -> crate::anthropic::TurnResponse {
    crate::anthropic::TurnResponse {
        assistant_content: text.into(),
        tool_uses: vec![],
        stop_reason: StopReason::EndTurn,
        usage,
        request_metadata: Default::default(),
    }
}

fn tool_use(t: Tool) -> crate::anthropic::TurnResponse {
    crate::anthropic::TurnResponse {
        assistant_content: String::new(),
        tool_uses: vec![(Uuid::new_v4(), t)],
        stop_reason: StopReason::ToolUse,
        usage: Usage::default(),
        request_metadata: Default::default(),
    }
}

/// Return the tempdir's RAII guard alongside the service so the
/// caller binds it for the duration of the test. Cleans up normally
/// when the test exits.
async fn make_service(
    scripted: Vec<crate::anthropic::TurnResponse>,
) -> (ConversationService, Arc<tempfile::TempDir>) {
    let dir = Arc::new(tempfile::tempdir().unwrap());
    let store = SessionStore::open(dir.path()).await.unwrap();
    let svc =
        ConversationService::new(Arc::new(MockLlmBackend::new(scripted)), store, config_dir());
    (svc, dir)
}

#[tokio::test]
async fn start_session_returns_greeting() {
    let (svc, _env) = make_service(vec![]).await;
    let (id, greeting) = svc.start_session(false).await.unwrap();
    assert_eq!(greeting.role, TurnRole::Assistant);
    assert!(svc.get_session(id).await.is_some());
}

#[tokio::test]
async fn send_turn_drives_tool_loop_to_end() {
    // Script: append_intake_prose tool → final assistant text
    let (svc, _env) = make_service(vec![
        tool_use(Tool::Batchable(BatchableTool::AppendIntakeProse {
            prose: "single cell scRNA-seq human samples".into(),
        })),
        assistant("Got it — looks like single-cell RNA-seq."),
    ])
    .await;
    let (id, _) = svc.start_session(false).await.unwrap();
    let turn = svc
        .send_turn(id, "tell me more".into(), None)
        .await
        .unwrap();
    assert!(turn.content.contains("single-cell"));
    let session = svc.get_session(id).await.unwrap();
    // Closure Phase B.3 — v4 archetypes now surface `discover_<axis>`
    // companions via the post-pass synthesis for every operation atom
    // with `method_choice` / `candidate_tools`. The single_cell_de
    // archetype's operation atoms (alignment, batch_correction,
    // Clustering,...) carry candidate_tools, so the rebuild produces
    // discover_* tasks and the session advances to `IntakeFollowup`.
    assert_eq!(session.state, SessionState::IntakeFollowup);
    assert!(session.taxonomy.is_some());
}

#[tokio::test]
async fn merge_preserves_current_dag_task_state_when_workflow_unchanged() {
    // the same-workflow_id merge branch in send_turn is a
    // deliberate "keep current, drop local" for task states. This
    // test proves that invariant: while a tool loop is "running",
    // a concurrent write to the persisted DAG (simulating a
    // harness progress event mid-turn) is preserved through the
    // merge, even though the local copy still carries the older
    // state snapshot.
    use ecaa_workflow_core::dag::TaskState;
    let (svc, _env) = make_service(vec![
        tool_use(Tool::Batchable(BatchableTool::AppendIntakeProse {
            prose: "single cell scRNA-seq human samples".into(),
        })),
        assistant("ok."),
    ])
    .await;
    let (id, _) = svc.start_session(false).await.unwrap();
    // First turn: builds the DAG.
    svc.send_turn(id, "set it up".into(), None).await.unwrap();
    let post_setup = svc.get_session(id).await.unwrap();
    // Phase D refactor: dag is derived; read via current_dag().
    let dag = post_setup.current_dag().expect("dag built");
    let any_task_id = dag.tasks.keys().next().expect("dag has tasks").clone();

    // Simulate a concurrent harness progress landing mid-turn by
    // writing the task's runtime state via the new task_states
    // authoritative map. Pre-Phase-D, this test wrote into
    // `session.dag.tasks[id].state` directly; post-Phase-D, the
    // authority is `session.task_states` and `current_dag()`
    // overlays at read time.
    let concurrent_completed = serde_json::json!({"concurrent": "harness-write"});
    let target_state = TaskState::Completed {
        result: concurrent_completed.clone(),
    };
    let target_state_clone = target_state.clone();
    svc.store_handle()
        .update(id, move |s| {
            s.set_task_state(any_task_id.as_str(), target_state_clone);
            Ok(())
        })
        .await
        .unwrap();

    // Drive a second turn. The send_turn merge must union the
    // harness's task_states write with anything the tool loop wrote;
    // the concurrent Completed entry must survive.
    let svc2 = svc; // reuse
                    // Need a second scripted turn.
    let svc2_mock = ConversationService::new(
        Arc::new(MockLlmBackend::new(vec![assistant("ack.")])),
        svc2.store_handle().clone(),
        config_dir(),
    );
    svc2_mock
        .send_turn(id, "continue".into(), None)
        .await
        .unwrap();

    let after = svc2_mock.get_session(id).await.unwrap();
    let dag = after.current_dag().expect("dag still present");
    // Re-read any_task_id from the post-merge dag — task ids are
    // stable across same-modality rebuilds.
    let any_task_id_after = dag.tasks.keys().next().expect("dag has tasks").clone();
    let task = dag
        .tasks
        .get(&any_task_id_after)
        .expect("task still present");
    match &task.state {
        TaskState::Completed { result } => {
            assert_eq!(
                result, &concurrent_completed,
                "merge must preserve the concurrent harness write"
            );
        }
        other => panic!(
            "expected Completed (concurrent write preserved), got {:?}",
            other
        ),
    }
}

#[tokio::test]
async fn confirm_advances_state() {
    let (svc, _env) = make_service(vec![
        tool_use(Tool::Batchable(BatchableTool::AppendIntakeProse {
            prose: "single cell scRNA-seq human samples".into(),
        })),
        tool_use(Tool::Batchable(BatchableTool::ProposeSummaryConfirmation {
            summary_markdown: "Here is the plan…".into(),
        })),
        assistant("Take a look and click Confirm when ready."),
    ])
    .await;
    let (id, _) = svc.start_session(false).await.unwrap();
    let _ = svc.send_turn(id, "go".into(), None).await.unwrap();
    let session = svc.get_session(id).await.unwrap();
    assert_eq!(
        session.state,
        SessionState::PendingConfirmation { stage: None }
    );
    assert_eq!(
        session.pending_emission_id,
        Some(uuid::Uuid::new_v5(
            &uuid::Uuid::NAMESPACE_OID,
            session.current_summary_hash().as_bytes(),
        )),
        "the persisted plan must retain the exact content-addressed identity shown on the card"
    );

    svc.confirm(id).await.unwrap();
    let session = svc.get_session(id).await.unwrap();
    assert_eq!(session.state, SessionState::ReadyToEmit);
    // `user_confirmed: true` replaced by `is_confirmed()` against the
    // per-emit token. `confirm_with_modes` mints a token bound to the
    // pending_emission_id + current summary hash.
    assert!(
        session.is_confirmed(),
        "confirm must arm the per-emit ConfirmationToken latch"
    );
    assert!(
        session.confirmation_token.is_some(),
        "confirm must mint a token"
    );
    assert!(
        session.pending_emission_id.is_some(),
        "confirm must retain a pending_emission_id"
    );
}

#[tokio::test]
async fn confirm_binds_selected_discipline_and_rejects_invalid_pair_without_mutation() {
    use ecaa_workflow_core::checkpoint_mode::CheckpointMode;
    use ecaa_workflow_core::session_mode::SessionMode;

    let scripted_confirmation = || {
        vec![
            tool_use(Tool::Batchable(BatchableTool::AppendIntakeProse {
                prose: "single cell scRNA-seq human samples".into(),
            })),
            tool_use(Tool::Batchable(BatchableTool::ProposeSummaryConfirmation {
                summary_markdown: "Review the complete plan.".into(),
            })),
            assistant("Confirm when ready."),
        ]
    };

    let (svc, _env) = make_service(scripted_confirmation()).await;
    let (id, _) = svc.start_session(false).await.unwrap();
    svc.send_turn(id, "prepare the analysis".into(), None)
        .await
        .unwrap();
    svc.confirm_with_modes(
        id,
        None,
        Some(SessionMode::Exploratory),
        Some(CheckpointMode::Selective),
    )
    .await
    .unwrap();
    let confirmed = svc.get_session(id).await.unwrap();
    let canonical_hash = confirmed.current_summary_hash();
    assert_eq!(confirmed.checkpoint_mode, CheckpointMode::Selective);
    assert_eq!(
        confirmed
            .confirmation_token
            .as_ref()
            .map(|token| token.summary_hash.as_str()),
        Some(canonical_hash.as_str()),
        "confirmation token must bind the plan after applying the selected discipline"
    );
    assert_eq!(
        confirmed.pending_emission_id,
        Some(uuid::Uuid::new_v5(
            &uuid::Uuid::NAMESPACE_OID,
            canonical_hash.as_bytes(),
        )),
        "emission identity must derive from the same post-selection plan"
    );

    let (invalid_svc, _invalid_env) = make_service(scripted_confirmation()).await;
    let (invalid_id, _) = invalid_svc.start_session(false).await.unwrap();
    invalid_svc
        .send_turn(invalid_id, "prepare the analysis".into(), None)
        .await
        .unwrap();
    let before = invalid_svc.get_session(invalid_id).await.unwrap();
    let invalid_mode = SessionMode::Confirmatory {
        prespecified_stages: vec!["differential_expression".into()],
        prespecified_parameters: Default::default(),
    };
    assert!(
        invalid_svc
            .confirm_with_modes(
                invalid_id,
                None,
                Some(invalid_mode),
                Some(CheckpointMode::Fast),
            )
            .await
            .is_err(),
        "confirmatory plus fast must be rejected"
    );
    let after = invalid_svc.get_session(invalid_id).await.unwrap();
    assert_eq!(after.state, before.state);
    assert_eq!(after.mode, before.mode);
    assert_eq!(after.checkpoint_mode, before.checkpoint_mode);
    assert_eq!(after.pending_emission_id, before.pending_emission_id);
    assert!(after.confirmation_token.is_none());
    assert!(!after.mode_locked);

    let (stale_svc, _stale_env) = make_service(scripted_confirmation()).await;
    let (stale_id, _) = stale_svc.start_session(false).await.unwrap();
    stale_svc
        .send_turn(stale_id, "prepare the analysis".into(), None)
        .await
        .unwrap();
    let pending_before_drift = stale_svc.get_session(stale_id).await.unwrap();
    stale_svc
        .store_handle()
        .update(stale_id, |session| {
            session
                .intake_prose
                .push_str(" Include a materially different endpoint.");
            Ok(())
        })
        .await
        .unwrap();
    assert!(
        stale_svc
            .confirm_with_modes(stale_id, None, None, None)
            .await
            .is_err(),
        "a card raised for an older plan fingerprint must fail closed"
    );
    let stale_after = stale_svc.get_session(stale_id).await.unwrap();
    assert_eq!(stale_after.state, pending_before_drift.state);
    assert_eq!(
        stale_after.pending_emission_id,
        pending_before_drift.pending_emission_id
    );
    assert!(stale_after.confirmation_token.is_none());
}

#[tokio::test]
async fn reject_returns_to_intake_preserving_methods() {
    // Phase B4 — uses a stage id (`batch_correction`) that the v4
    // single_cell_de archetype's discover-companion synthesis produces.
    // Pre-B4 this test pinned `composer_version=1` to hit the legacy
    // taxonomy build's `preprocessing` stage; with the v1 fallback
    // retired, the test uses a v4-supported stage instead.
    let (svc, _env) = make_service(vec![
        tool_use(Tool::Batchable(BatchableTool::AppendIntakeProse {
            prose: "single cell scRNA-seq human samples".into(),
        })),
        tool_use(Tool::Batchable(BatchableTool::SetIntakeMethod {
            stage: "batch_correction".into(),
            method_prose: "Seurat v5 CCA".into(),
        })),
        tool_use(Tool::Batchable(BatchableTool::ProposeSummaryConfirmation {
            summary_markdown: "Plan ready".into(),
        })),
        assistant("Confirm?"),
    ])
    .await;
    let (id, _) = svc.start_session(false).await.unwrap();
    // Simulate the UI affordance that flips the
    // SME-named flag before the LLM's set_intake_method tool fires.
    // Without this signal the gate refuses the dispatch.
    svc.store_handle()
        .update(id, |s| {
            s.sme_method_signals
                .named
                .insert("batch_correction".into(), true);
            Ok(())
        })
        .await
        .unwrap();
    let _ = svc.send_turn(id, "go".into(), None).await.unwrap();
    let session = svc.get_session(id).await.unwrap();
    assert!(session.intake_methods.0.contains_key("batch_correction"));
    assert_eq!(
        session.state,
        SessionState::PendingConfirmation { stage: None }
    );

    svc.reject(id).await.unwrap();
    let session = svc.get_session(id).await.unwrap();
    assert_eq!(session.state, SessionState::Intake);
    assert!(session.intake_methods.0.contains_key("batch_correction"));
}

/// Reject must clear the confirmation token (replaces the legacy
/// `user_confirmed` bool) so a later emit_package tool call cannot
/// piggyback on a stale latch. The state machine only allows reject
/// from PendingConfirmation (not ReadyToEmit), so the regression case
/// is a session at PendingConfirmation with a dangling token from any
/// prior cycle: the reject must zero it regardless of how it got set.
#[tokio::test]
async fn reject_resets_user_confirmed() {
    let (svc, _env) = make_service(vec![
        tool_use(Tool::Batchable(BatchableTool::AppendIntakeProse {
            prose: "single cell scRNA-seq human samples".into(),
        })),
        tool_use(Tool::Batchable(BatchableTool::ProposeSummaryConfirmation {
            summary_markdown: "Plan ready".into(),
        })),
        assistant("Confirm?"),
    ])
    .await;
    let (id, _) = svc.start_session(false).await.unwrap();
    let _ = svc.send_turn(id, "go".into(), None).await.unwrap();
    let session = svc.get_session(id).await.unwrap();
    assert_eq!(
        session.state,
        SessionState::PendingConfirmation { stage: None }
    );
    assert!(
        !session.is_confirmed(),
        "fresh session has no latched confirm"
    );

    // Simulate a stale latch by directly minting a ConfirmationToken
    // while the session is in PendingConfirmation. This mirrors the
    // threat model: any path that leaves the latch armed when the SME
    // has not yet clicked Confirm on the current card. The reject path
    // must clear it regardless of how it got set. Replaces older
    // `s.user_confirmed = true` by minting a token against a synthetic
    // pending_emission_id.
    svc.store_handle()
        .update(id, |s| {
            s.pending_emission_id = Some(uuid::Uuid::new_v4());
            let _ = s.mint_confirmation_token(
                chrono::Utc::now(),
                crate::audit_actor::AuditActor::User("test".into()),
            );
            Ok(())
        })
        .await
        .unwrap();

    // SME clicks Reject. The confirmation token must clear so an LLM
    // emit_package call from the next turn cannot ride on the stale
    // authorization.
    svc.reject(id).await.unwrap();
    let session = svc.get_session(id).await.unwrap();
    assert_eq!(session.state, SessionState::Intake);
    assert!(
        session.confirmation_token.is_none(),
        "reject must clear the confirmation token (F-CONC-M-2 + C2)"
    );
    assert!(
        session.pending_emission_id.is_none(),
        "reject must clear pending_emission_id so the next confirm \
         mints a fresh uuid (P0-203 / C2)"
    );
    assert!(!session.is_confirmed(), "reject must un-arm the latch");
}

#[tokio::test]
async fn repeated_tool_calls_resolve_to_acknowledge_when_model_finally_ends() {
    // The tool loop processes each tool_use the model returns and
    // only exits with Acknowledge when the model finally emits a
    // plain assistant response. Iteration budget is TOOL_LOOP_CAP
    // (10); a reasonable script stays well under it.
    let mut script: Vec<crate::anthropic::TurnResponse> = (0..5)
        .map(|_| {
            tool_use(Tool::Batchable(BatchableTool::ClassifyIntake {
                prose: "x".into(),
            }))
        })
        .collect();
    script.push(assistant("end"));
    let (svc, _env) = make_service(script).await;
    let (id, _) = svc.start_session(false).await.unwrap();
    let turn = svc.send_turn(id, "go".into(), None).await.unwrap();
    assert_eq!(
        turn.intent,
        Some(crate::session::AssistantIntent::Acknowledge)
    );
}

#[tokio::test]
async fn auto_append_hydrates_session_before_tool_loop() {
    // The mock LLM returns a plain text response without calling any
    // tools. Before the auto-append fix, this would leave
    // intake_prose empty because the LLM never called
    // append_intake_prose. After the fix, send_turn pre-hydrates the
    // session so the LLM can respond directly with EndTurn.
    let (svc, _env) = make_service(vec![assistant(
        "Got it — single-cell RNA-seq on human IVD tissue.",
    )])
    .await;
    let (id, _) = svc.start_session(false).await.unwrap();
    let turn = svc
        .send_turn(
            id,
            "single cell scRNA-seq from human intervertebral disc".into(),
            None,
        )
        .await
        .unwrap();
    let session = svc.get_session(id).await.unwrap();

    assert!(!session.intake_prose.is_empty());
    assert!(session.classification.is_some());
    assert_ne!(session.state, SessionState::Greeting);
    // Phase D refactor: session.dag is a derived cache; readers
    // must call current_dag() which lowers workflow_dag on demand.
    assert!(session.current_dag().is_some());
    assert!(turn.content.contains("single-cell"));
}

#[tokio::test]
async fn auto_append_skips_when_intake_prose_already_set() {
    // A follow-up turn to an already-populated session should NOT
    // double-auto-append — the guard checks intake_prose.is_empty().
    let (svc, _env) = make_service(vec![
        tool_use(Tool::Batchable(BatchableTool::AppendIntakeProse {
            prose: "single cell scRNA-seq human samples".into(),
        })),
        assistant("Got it."),
        // Second turn — the LLM just responds with text, no tool calls.
        assistant("What tissue?"),
    ])
    .await;
    let (id, _) = svc.start_session(false).await.unwrap();

    // First turn — auto-append fires (intake_prose is empty).
    let _ = svc.send_turn(id, "scRNA-seq".into(), None).await.unwrap();
    let after_first = svc.get_session(id).await.unwrap();
    let prose_len_after_first = after_first.intake_prose.len();

    // Second turn — auto-append must NOT fire (intake_prose is non-empty).
    // The mock LLM just returns text, so no tool appends either.
    let _ = svc
        .send_turn(id, "more details".into(), None)
        .await
        .unwrap();
    let after_second = svc.get_session(id).await.unwrap();
    // intake_prose should not have grown from the auto-append path.
    assert_eq!(after_second.intake_prose.len(), prose_len_after_first);
}

#[tokio::test]
async fn quick_reply_directive_propagates_to_final_turn() {
    let (svc, _env) = make_service(vec![
        tool_use(Tool::Batchable(BatchableTool::ProposeQuickReplies {
            question: "Which species?".into(),
            options: vec!["human".into(), "mouse".into()],
        })),
        assistant("Quick question:"),
    ])
    .await;
    let (id, _) = svc.start_session(false).await.unwrap();
    let turn = svc.send_turn(id, "begin".into(), None).await.unwrap();
    assert_eq!(
        turn.quick_replies,
        vec!["human".to_string(), "mouse".to_string()]
    );
}

#[tokio::test]
async fn send_turn_accumulates_usage_from_every_loop_iteration() {
    let usage_one = Usage {
        input_tokens: 150,
        output_tokens: 30,
        cache_read_input_tokens: 200,
        cache_creation_input_tokens: 1500,
    };
    let usage_two = Usage {
        input_tokens: 220,
        output_tokens: 45,
        cache_read_input_tokens: 1700,
        cache_creation_input_tokens: 0,
    };
    let mut first = tool_use(Tool::Batchable(BatchableTool::AppendIntakeProse {
        prose: "single cell".into(),
    }));
    first.usage = usage_one;
    let (svc, _env) = make_service(vec![
        first,
        assistant_with_usage("ok, recorded.", usage_two),
    ])
    .await;
    let (id, _) = svc.start_session(false).await.unwrap();
    let _ = svc.send_turn(id, "go".into(), None).await.unwrap();
    let metrics = svc.metrics_snapshot(id).await.unwrap();
    assert_eq!(metrics.total_input_tokens, 370);
    assert_eq!(metrics.total_output_tokens, 75);
    assert_eq!(metrics.cache_read_tokens, 1900);
    assert_eq!(metrics.cache_creation_tokens, 1500);
    assert_eq!(metrics.turn_count, 1);
}

// D8 mitigation — regression tests for the Anthropic
// request-body-timeout path. Verifies (1) a timeout-marker error
// surfaces as `ServiceError::Backend` carrying the marker (the UI's
// signal to render a "request stalled" affordance instead of a generic
// failure), AND (2) the marker is classified as terminal so we do NOT
// burn the MAX_RETRIES_PER_TURN=2 budget against an already-hung
// backend. Both behaviours land at the conversation layer — no live
// Anthropic call needed.

/// `LlmBackend` that always returns the request-body timeout error
/// shape `AnthropicClient::send_turn` produces when reqwest's
/// per-request timeout fires. Counts invocations so the test can
/// assert "exactly one attempt" (no retries).
struct TimeoutMockBackend {
    attempts: std::sync::atomic::AtomicU32,
}

impl TimeoutMockBackend {
    fn new() -> Self {
        Self {
            attempts: std::sync::atomic::AtomicU32::new(0),
        }
    }
    fn attempts(&self) -> u32 {
        self.attempts.load(std::sync::atomic::Ordering::SeqCst)
    }
}

#[async_trait::async_trait]
impl crate::anthropic::LlmBackend for TimeoutMockBackend {
    async fn send_turn(
        &self,
        _request: crate::anthropic::TurnRequest,
    ) -> anyhow::Result<crate::anthropic::TurnResponse> {
        self.attempts
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Err(anyhow::anyhow!(
            "{} after 180s on POST https://api.anthropic.com/v1/messages: operation timed out",
            crate::anthropic::client::REQUEST_BODY_TIMEOUT_MARKER
        ))
    }
}

#[tokio::test]
async fn request_body_timeout_surfaces_as_backend_error_without_retry() {
    let backend = Arc::new(TimeoutMockBackend::new());
    let dir = Arc::new(tempfile::tempdir().unwrap());
    let store = SessionStore::open(dir.path()).await.unwrap();
    let svc = ConversationService::new(backend.clone(), store, config_dir());
    let (id, _) = svc.start_session(false).await.unwrap();

    let err = svc
        .send_turn(id, "hello".into(), None)
        .await
        .expect_err("timeout must surface as an error, not a silent hang");

    // The UI signals timeout-vs-generic-failure on the marker substring.
    // Stringify via `format!` because ServiceError carries the wrapped
    // anyhow message inside its `Backend(String)` variant.
    let msg = format!("{}", err);
    assert!(
        msg.contains(crate::anthropic::client::REQUEST_BODY_TIMEOUT_MARKER),
        "Backend error must carry the request-body-timeout marker so the \
         UI can distinguish it from a generic 5xx; got: {}",
        msg
    );

    // The point of the explicit terminal-classification: don't burn
    // additional 180s windows against an already-hung backend. Exactly
    // one attempt — no retries.
    assert_eq!(
        backend.attempts(),
        1,
        "request-body timeout must NOT trigger the standard 2-retry policy; \
         that would compound user-visible latency on a stuck call"
    );
}

/// Build a service whose session carries a composed DAG (classification +
/// taxonomy present, so `amend`'s internal `rebuild_dag` succeeds) forced
/// into `Emitted`, plus a known stage id to amend. Mirrors the tools-test
/// approach in
/// `tools/tests.rs::amend_stage_method_invalidates_slice_and_advances_to_ready_to_emit`:
/// build the DAG via AppendIntakeProse, then force `Emitted` directly.
/// Returns the tempdir guard so the backing store outlives the test.
async fn emit_test_session() -> (
    ConversationService,
    Arc<tempfile::TempDir>,
    crate::session::SessionId,
    String,
) {
    let dir = Arc::new(tempfile::tempdir().unwrap());
    let store = SessionStore::open(dir.path()).await.unwrap();
    let svc = ConversationService::new(
        Arc::new(MockLlmBackend::new(vec![
            tool_use(Tool::Batchable(BatchableTool::AppendIntakeProse {
                prose: "bulk rna-seq differential expression in human samples".into(),
            })),
            assistant("ok."),
        ])),
        store,
        config_dir(),
    );
    let (id, _) = svc.start_session(false).await.unwrap();
    svc.send_turn(id, "set it up".into(), None).await.unwrap();
    let session = svc
        .store_handle()
        .update(id, |s| {
            s.state = SessionState::Emitted;
            // A real emit warms the `session.dag` cache; force it here so
            // `amend_stage_method` (which reads `session.dag` directly)
            // sees the composed plan, matching a genuine Emitted session.
            s.ensure_dag_cached();
            Ok(())
        })
        .await
        .unwrap();
    // `amend_stage_method` validates the stage against `session.dag`
    // (the composer output), not the lowered `current_dag()` overlay, so
    // pick the target from the same map amend checks.
    let dag = session.dag.as_ref().expect("dag built during intake");
    let stage = dag
        .tasks
        .keys()
        .find(|k| {
            k.as_str().starts_with("differential_expression")
                || k.as_str().starts_with("normalization")
        })
        .map(|k| k.to_string())
        .unwrap_or_else(|| dag.tasks.keys().next().expect("dag has tasks").to_string());
    (svc, dir, id, stage)
}

/// The REST amend wrapper has no LLM dispatcher to drain the
/// `AmendStart`/`AmendReady` triggers the tool body defers, so it must
/// drain them itself, then re-raise the summary confirmation card and move
/// to `PendingConfirmation` so the SME's `/confirm` can re-emit the amended
/// plan (FIX 1). Without the drain the session sticks in `Emitted` with two
/// queued triggers; without the card + PendingConfirmation the `/confirm`
/// round-trip returned 400 and nothing re-emitted.
#[tokio::test]
async fn rest_amend_raises_confirmation_card_and_drains_triggers() {
    let (svc, _env, session_id, stage) = emit_test_session().await;
    svc.amend_stage_method_from_rest(
        session_id,
        stage.clone(),
        "star".into(),
        Some("try STAR".into()),
    )
    .await
    .expect("amend ok");
    let s = svc.get_session(session_id).await.expect("session");
    assert!(
        matches!(s.state, SessionState::PendingConfirmation { .. }),
        "REST amend must re-raise the confirmation card (PendingConfirmation), got {:?}",
        s.state
    );
    assert!(
        s.conversation
            .iter()
            .rev()
            .any(|t| t.confirmation_card.is_some()),
        "REST amend must raise a confirmation card on the conversation tail"
    );
    assert!(
        !s.is_confirmed(),
        "confirmation discipline: no token is minted until a real SME /confirm"
    );
    assert!(
        s.deferred_state_triggers.is_empty(),
        "deferred triggers must be drained after a REST amend, got {:?}",
        s.deferred_state_triggers
    );
}

/// Task 5.1 — a branch of an ALREADY-EMITTED parent must NOT auto-emit.
/// The child surfaces a confirmation card (PendingConfirmation) with no
/// `System` confirmation token and no emitted package, so the SME must
/// explicitly confirm before the child emits (branch-to-edit may have
/// changed the plan).
#[tokio::test]
async fn branch_of_emitted_parent_stages_confirmation_without_auto_emit() {
    let (svc, _env, parent_id, _stage) = emit_test_session().await;
    // Make the parent look genuinely emitted so `should_emit_child_package`
    // fires: the parent has a package path and the child inherits the
    // composed workflow_dag.
    svc.store_handle()
        .update(parent_id, |s| {
            s.emitted_package_path = Some(std::path::PathBuf::from("/nonexistent/parent-pkg"));
            assert!(
                s.workflow_dag.is_some(),
                "parent must carry a composed workflow_dag for this test"
            );
            Ok(())
        })
        .await
        .unwrap();

    let child_id = svc
        .branch_session_with_rationale_and_task(parent_id, false, None, None)
        .await
        .expect("branch ok");
    let child = svc.get_session(child_id).await.expect("child session");
    assert!(
        matches!(child.state, SessionState::PendingConfirmation { .. }),
        "branch of an emitted parent must stage the child at PendingConfirmation, got {:?}",
        child.state
    );
    assert!(
        child
            .conversation
            .iter()
            .rev()
            .any(|t| t.confirmation_card.is_some()),
        "branch child must surface a confirmation card the SME can click"
    );
    assert!(
        child.emitted_package_path.is_none(),
        "branch child must NOT auto-emit a package before an explicit SME confirm"
    );
    assert!(
        !child.is_confirmed(),
        "branch child must NOT carry a System confirmation token (no auto-emit gate open)"
    );
}

/// FIX 1 (the audit regression) — the FULL post-edit re-emit round-trip. An SME
/// REST edit raises a confirmation card + moves to PendingConfirmation;
/// `/confirm` (confirm_with_modes) then drives PendingConfirmation → ReadyToEmit
/// WITHOUT the "illegal session transition: UserClickedConfirm from ReadyToEmit"
/// 400 the audit hit, and `try_auto_emit_after_confirm` re-emits a package whose
/// on-disk `policies/validation-contract.json` reflects the SME edit. Before the
/// fix this test fails at the `confirm_with_modes` step (400) and never
/// re-emits.
#[tokio::test]
#[serial_test::serial]
async fn rest_edit_then_confirm_re_emits_and_reflects_edit_on_disk() {
    let pkg_root = tempfile::tempdir().unwrap();
    let prev_root = std::env::var("ECAA_PACKAGE_ROOT").ok();
    std::env::set_var("ECAA_PACKAGE_ROOT", pkg_root.path());

    let (svc, _env, id, stage) = emit_test_session().await;
    // Key the bound onto the target DAG stage. v4 stages carry no
    // `spec.stage_class`, so the harness matches the contract block by the bare
    // task id — use that as the stage_class (FIX 3 accepts a task-id match).
    let stage_class = stage.clone();

    let bound = ecaa_workflow_core::validation_bound::SmeValidationBound {
        stage_class: stage_class.clone(),
        assertion_type: "numeric_threshold".into(),
        target: "results/tables/de.json".into(),
        check: serde_json::json!({ "json_pointer": "/adjusted_p_max", "op": "lte", "value": 0.01 }),
        severity: "required".into(),
        id: "sme_roundtrip_padj".into(),
        description: "SME: adjusted p must be <= 0.01".into(),
    };
    svc.set_validation_bound_from_rest(
        id,
        stage_class.clone(),
        Some(bound),
        "sme_roundtrip_padj".into(),
        None,
    )
    .await
    .expect("set_validation_bound must succeed");

    // The edit re-raises the confirmation card + moves to PendingConfirmation.
    let s = svc.get_session(id).await.unwrap();
    assert!(
        matches!(s.state, SessionState::PendingConfirmation { .. }),
        "post-edit session must be PendingConfirmation, got {:?}",
        s.state
    );

    // The exact audit regression: confirm must NOT 400.
    svc.confirm_with_modes(id, None, None, None)
        .await
        .expect("confirm from PendingConfirmation must succeed (no illegal-transition 400)");
    let outcome = svc
        .try_auto_emit_after_confirm(id)
        .await
        .expect("auto-emit call ok");
    assert!(
        outcome.is_some(),
        "auto-emit must fire after the SME confirm"
    );

    let s = svc.get_session(id).await.unwrap();
    assert!(
        matches!(s.state, SessionState::Emitted),
        "session must reach Emitted after confirm + auto-emit, got {:?}",
        s.state
    );
    let pkg = s
        .emitted_package_path
        .clone()
        .expect("emitted package path must be set after auto-emit");
    assert!(
        pkg.starts_with(pkg_root.path()),
        "re-emit must land under the test package root, got {}",
        pkg.display()
    );

    // The on-disk re-emitted contract must carry the SME bound.
    let contract: serde_json::Value = serde_json::from_slice(
        &std::fs::read(pkg.join("policies/validation-contract.json"))
            .expect("re-emitted package must carry policies/validation-contract.json"),
    )
    .unwrap();
    let found = contract["stages"][&stage_class]["assertions"]
        .as_array()
        .map(|a| a.iter().any(|x| x["id"] == "sme_roundtrip_padj"))
        .unwrap_or(false);
    assert!(
        found,
        "the re-emitted contract must reflect the SME bound edit, got {contract}"
    );

    match prev_root {
        Some(v) => std::env::set_var("ECAA_PACKAGE_ROOT", v),
        None => std::env::remove_var("ECAA_PACKAGE_ROOT"),
    }
}
