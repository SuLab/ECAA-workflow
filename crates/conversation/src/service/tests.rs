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

    svc.confirm(id).await.unwrap();
    let session = svc.get_session(id).await.unwrap();
    assert_eq!(session.state, SessionState::ReadyToEmit);
    // `user_confirmed: true` replaced by `is_confirmed()` against the
    // per-emit token. `confirm_with_modes` mints a token bound to the
    // pending_emission_id (synthesized when missing) + current summary
    // hash.
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
        "confirm must seed a pending_emission_id when missing"
    );
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
