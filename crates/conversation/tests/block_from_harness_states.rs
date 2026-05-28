//! Regression: `ConversationService::block_from_harness` must accept the
//! same execution-side states as the state-machine's HarnessTaskBlocked
//! trigger, not only the older Emitted/Intake whitelist.

use ecaa_workflow_conversation::{
    ConversationService, LlmBackend, MockLlmBackend, Session, SessionState, SessionStore,
};
use ecaa_workflow_core::blocker::BlockerKind;
use std::path::PathBuf;
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

async fn service_with_session(state: SessionState) -> (ConversationService, uuid::Uuid, TestEnv) {
    let env = TestEnv::new();
    let store = SessionStore::open(env.path()).await.unwrap();
    let backend: Arc<dyn LlmBackend> = Arc::new(MockLlmBackend::new(vec![]));
    let service = ConversationService::new(backend, store.clone(), config_dir());
    let mut session = Session::new(false);
    session.state = state;
    let id = session.id;
    store.save(&session).await.unwrap();
    (service, id, env)
}

async fn assert_blocks_from(state: SessionState) {
    // `_env` keeps the tempdir alive (RAII via Arc<TempDir>) until the
    // test exits. Drop here removes the dir; no leak.
    let (service, id, _env) = service_with_session(state).await;
    service
        .block_from_harness(
            id,
            "task-a".into(),
            "needs input".into(),
            BlockerKind::AgentError {
                message: "needs input".into(),
            },
        )
        .await
        .unwrap();

    let session = service.get_session(id).await.unwrap();
    assert!(
        matches!(session.state, SessionState::Blocked { .. }),
        "block_from_harness should transition execution-side state to Blocked, got {:?}",
        session.state
    );
}

#[tokio::test]
async fn block_from_harness_accepts_ready_to_emit() {
    assert_blocks_from(SessionState::ReadyToEmit).await;
}

#[tokio::test]
async fn block_from_harness_accepts_amending() {
    assert_blocks_from(SessionState::Amending {
        target_stage: "alignment".into(),
        invalidated_tasks: vec!["alignment".into()],
    })
    .await;
}

#[tokio::test]
async fn block_from_harness_refreshes_already_blocked() {
    assert_blocks_from(SessionState::Blocked {
        blockers: vec![],
        reason: "prior".into(),
        recovery_hint: "retry".into(),
        blocker_kind: None,
        context: None,
    })
    .await;
}
