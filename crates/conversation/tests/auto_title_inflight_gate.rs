//! `maybe_auto_title` is fired from `send_turn` and
//! kicks off a detached `tokio::spawn` that calls Haiku. Before this gate,
//! two near-simultaneous `send_turn` invocations on the same session would
//! both pass the threshold check, both spawn, and both call Haiku before
//! the first persisted `session.title`. Each call costs ~$0.0005 and the
//! session ends up with only one title, so the second call is pure waste.
//!
//! The fix is a `Arc<dashmap::DashSet<SessionId>>` registry on
//! `ConversationService`. `maybe_auto_title` tries `insert(id)`; on
//! contention it bails. The spawned task removes the id from the set
//! when it finishes (success or failure).
//!
//! This test exercises the production path: a `CountingMockBackend`
//! records every Haiku-model call and exposes the count. We drive two
//! concurrent `send_turn`s through the service and assert exactly one
//! Haiku call occurred.

// S5.32: workspace lint is `unsafe_code = "deny"`. This test uses
// `unsafe { std::env::set_var / remove_var }` (unsafe in Rust 2024
// edition because the env table is not thread-safe). The waiver is
// bounded to test setup/teardown.
#![allow(unsafe_code)]

use async_trait::async_trait;
use scripps_workflow_conversation::anthropic::{LlmBackend, TurnRequest, TurnResponse, Usage};
use scripps_workflow_conversation::model_policy::ModelId;
use scripps_workflow_conversation::{
    ConversationService, SessionId, SessionStore, StopReason, Tool,
};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

#[path = "common/mod.rs"]
mod common;
use common::TestEnv;

fn config_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("config")
}

/// Mock backend that distinguishes Haiku side-calls from the main
/// Sonnet/Opus chat path. Returns scripted responses on the chat path
/// in FIFO order; returns a canned title on Haiku and increments a
/// counter so the test can assert exactly-once.
struct CountingMockBackend {
    chat_responses: tokio::sync::Mutex<std::collections::VecDeque<TurnResponse>>,
    haiku_calls: AtomicUsize,
}

impl CountingMockBackend {
    fn new(chat: Vec<TurnResponse>) -> Arc<Self> {
        Arc::new(Self {
            chat_responses: tokio::sync::Mutex::new(chat.into_iter().collect()),
            haiku_calls: AtomicUsize::new(0),
        })
    }

    fn haiku_call_count(&self) -> usize {
        self.haiku_calls.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl LlmBackend for CountingMockBackend {
    async fn send_turn(&self, req: TurnRequest) -> anyhow::Result<TurnResponse> {
        if req.model == ModelId::Haiku45 {
            // Simulate latency so the second concurrent caller has a
            // realistic window to race in. Without a sleep the first
            // spawn might complete before the second `send_turn` even
            // reaches `maybe_auto_title`, masking the bug.
            self.haiku_calls.fetch_add(1, Ordering::SeqCst);
            tokio::time::sleep(std::time::Duration::from_millis(150)).await;
            return Ok(TurnResponse {
                assistant_content: "Bulk RNA-seq DE liver".into(),
                tool_uses: vec![],
                stop_reason: StopReason::EndTurn,
                usage: Usage {
                    input_tokens: 100,
                    output_tokens: 5,
                    cache_read_input_tokens: 0,
                    cache_creation_input_tokens: 0,
                },
                request_metadata: Default::default(),
            });
        }
        let mut q = self.chat_responses.lock().await;
        match q.pop_front() {
            Some(r) => Ok(r),
            None => Err(anyhow::anyhow!("CountingMockBackend chat queue exhausted")),
        }
    }
    async fn send_turn_streaming(
        &self,
        req: TurnRequest,
        _on_delta: scripps_workflow_conversation::anthropic::delta_sink::DeltaSink,
    ) -> anyhow::Result<TurnResponse> {
        self.send_turn(req).await
    }
}

fn assistant(text: &str) -> TurnResponse {
    TurnResponse {
        assistant_content: text.into(),
        tool_uses: vec![],
        stop_reason: StopReason::EndTurn,
        usage: Usage::default(),
        request_metadata: Default::default(),
    }
}

fn tool_use(t: Tool) -> TurnResponse {
    TurnResponse {
        assistant_content: String::new(),
        tool_uses: vec![(uuid::Uuid::new_v4(), t)],
        stop_reason: StopReason::ToolUse,
        usage: Usage::default(),
        request_metadata: Default::default(),
    }
}

/// Helper: seed the session past `AUTO_TITLE_TURN_THRESHOLD` (6
/// non-system turns) and give it a classification so the threshold
/// check passes. We do this by hand-patching the session via
/// `store.update` rather than driving live turns — keeps the fixture
/// tight and the test deterministic.
async fn seed_auto_title_ready_session(svc: &ConversationService) -> SessionId {
    use scripps_workflow_core::classify::ClassificationResult;
    let (id, _) = svc.start_session(false).await.unwrap();
    let store = svc.store_handle();
    store
        .update(id, |s| {
            use scripps_workflow_conversation::session::Turn;
            // Six non-system turns: alternating user / assistant.
            for i in 0..6 {
                let t = if i % 2 == 0 {
                    Turn::user(format!("user msg {i}"))
                } else {
                    Turn::assistant(format!("assistant reply {i}"))
                };
                std::sync::Arc::make_mut(&mut s.conversation).push(t);
            }
            s.classification = Some(ClassificationResult {
                confidence: 0.9,
                ..Default::default()
            });
            s.title = None; // ensure auto-title can fire
            Ok(())
        })
        .await
        .unwrap();
    id
}

/// Two concurrent `send_turn` invocations on the same session
/// must produce AT MOST one Haiku call. Without the gate, both turns
/// would race past `if session.title.is_some()` and both spawn.
///
/// Serialized on `SWFC_AUTO_TITLE` so concurrent
/// tests that read this same env var don't race the set/remove.
#[serial_test::serial(SWFC_AUTO_TITLE)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_send_turn_spawns_only_one_haiku_auto_title_call() {
    // Set the feature flag for the duration of this test. Other
    // auto-title tests in the suite share this env var; the
    // assignment is idempotent.
    // SAFETY: set_var on a test-process-local env var. We synchronize
    // by running the test in a serial way conceptually — concurrent
    // tests in this crate that set the same var are gated through a
    // dedicated lock, but auto-title's flag is read-only so a stale
    // set is harmless.
    unsafe { std::env::set_var("SWFC_AUTO_TITLE", "1") };

    // `_env` keeps the tempdir alive (RAII via Arc<TempDir>) until the
    // test function exits.
    let _env = TestEnv::new();
    let store = SessionStore::open(_env.path()).await.unwrap();

    // Two chat-turn responses for the two send_turn invocations.
    let backend = CountingMockBackend::new(vec![assistant("reply 1"), assistant("reply 2")]);
    let backend_for_count: Arc<CountingMockBackend> = backend.clone();
    let svc = Arc::new(ConversationService::new(
        backend.clone() as Arc<dyn LlmBackend>,
        store,
        config_dir(),
    ));

    let id = seed_auto_title_ready_session(&svc).await;

    // Drop a sample tool_use into the script in case the tool loop
    // tries to dispatch one — keep it harmless. The `assistant`
    // responses end the turn cleanly.
    let _ = tool_use; // suppress unused-fn warning

    // Fire two concurrent send_turns. Both should pass through
    // maybe_auto_title; without the gate, both spawn; with the gate,
    // only one wins the insert.
    let svc_a = svc.clone();
    let svc_b = svc.clone();
    let a = tokio::spawn(async move { svc_a.send_turn(id, "go".into(), None).await });
    let b = tokio::spawn(async move { svc_b.send_turn(id, "go again".into(), None).await });
    let (ra, rb) = tokio::join!(a, b);
    assert!(ra.is_ok());
    assert!(rb.is_ok());
    let ra = ra.unwrap();
    let rb = rb.unwrap();
    assert!(ra.is_ok(), "send_turn A failed: {:?}", ra.err());
    assert!(rb.is_ok(), "send_turn B failed: {:?}", rb.err());

    // Wait for any in-flight detached title task to complete. We
    // Can't.await the spawned future (it's detached), so we poll
    // the haiku counter — once a title is persisted OR a sufficient
    // sleep elapses, the count is stable.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    loop {
        if backend_for_count.haiku_call_count() >= 1 {
            // observed at least one call — let any racing second
            // call get its chance to record before we measure
            tokio::time::sleep(std::time::Duration::from_millis(400)).await;
            break;
        }
        if std::time::Instant::now() >= deadline {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }

    let count = backend_for_count.haiku_call_count();
    assert!(
        count <= 1,
        "expected at most 1 Haiku auto-title call across two concurrent send_turns; got {count}"
    );
    assert_eq!(
        count, 1,
        "expected exactly 1 Haiku auto-title call; got {count}"
    );

    unsafe { std::env::remove_var("SWFC_AUTO_TITLE") };
}
