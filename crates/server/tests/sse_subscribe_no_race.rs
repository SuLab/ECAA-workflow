//! RC-19 regression for the publish-before-subscribe window in
//! `events_stream`. A two-step `broadcaster(id).await` followed by
//! `tx.subscribe()` opens a window where any fanout between the two
//! calls lands on a sender with zero subscribers, returns
//! `Err(SendError)`, and the event is lost (SSE has no resync signal).
//! `broadcaster_subscribe(id).await` performs the get-or-insert +
//! subscribe atomically.
//!
//! This test demonstrates both halves of the contract: the two-step
//! path can lose events fired between the steps, and the atomic path
//! catches them.

use ecaa_workflow_conversation::{LlmBackend, MockLlmBackend, SessionStore};
use ecaa_workflow_server::chat_routes::{ChatAppState, SsePayload};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

fn config_dir() -> PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("config")
}

async fn build_state() -> ChatAppState {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = SessionStore::open(dir.path())
        .await
        .expect("open session store");
    std::mem::forget(dir);
    let backend: Arc<dyn LlmBackend> = Arc::new(MockLlmBackend::new(vec![]));
    ChatAppState::with_backend(backend, store, config_dir())
}

/// `broadcaster_subscribe` returns a receiver that observes any fanout
/// fired after it returns. Demonstrates the post-fix invariant: there
/// is no two-step window where a fanout can send to a zero-subscriber
/// sender.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn broadcaster_subscribe_observes_concurrent_fanout() {
    let app = build_state().await;
    let (session_id, _) = app
        .conversation
        .start_session(false)
        .await
        .expect("start_session");

    // Atomic subscribe: write-lock held across the entry-or-insert +
    // subscribe so any subsequent fanout sees the channel with our
    // receiver attached.
    let mut rx = app.broadcaster_subscribe(session_id).await;

    app.spawn_fanout(
        session_id,
        SsePayload::AssistantTokenDelta {
            text: "delta-after-subscribe".into(),
        },
    );

    // Let the spawned fanout run.
    tokio::time::sleep(Duration::from_millis(100)).await;

    match rx.try_recv() {
        Ok(envelope) => match envelope.payload {
            SsePayload::AssistantTokenDelta { text } => {
                assert_eq!(text, "delta-after-subscribe");
            }
            other => panic!(
                "expected AssistantTokenDelta after broadcaster_subscribe, got {:?}",
                other
            ),
        },
        other => panic!(
            "expected envelope with AssistantTokenDelta after broadcaster_subscribe, got {:?}",
            other
        ),
    }
}
