//! Regression for per-session SSE seq ordering (mint-at-send). The
//! spawned fanout path (`spawn_fanout` / `BroadcastEventSink::fanout`)
//! and the synchronous `broadcast` path share one per-session seq
//! counter. The earlier implementation minted the seq on the calling
//! thread BEFORE spawning the send, so a spawned fanout that minted
//! seq N could be overtaken on the wire by a synchronous broadcast that
//! minted seq N+1 and sent inline — delivering N+1 before N. The UI
//! drops any event with `seq <= lastSeq`, so the lower-seq spawned
//! event was silently lost.
//!
//! After the fix both paths mint the seq immediately before `tx.send`
//! on whichever executor performs the send, so seq assignment order
//! equals delivery order: the first event delivered always carries the
//! lower seq.

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
    // Leak so the tempdir outlives the store the app holds.
    std::mem::forget(dir);
    let backend: Arc<dyn LlmBackend> = Arc::new(MockLlmBackend::new(vec![]));
    ChatAppState::with_backend(backend, store, config_dir())
}

/// A spawned fanout immediately followed by a synchronous broadcast must
/// not let the synchronous event's seq beat the spawned event's seq on
/// the wire. With mint-at-send the first event delivered always carries
/// the lower seq, regardless of which executor runs the send first.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn rapid_fanout_then_sync_broadcast_preserves_seq_order() {
    let app = build_state().await;
    let (session_id, _greeting) = app
        .conversation
        .start_session(false)
        .await
        .expect("start_session");

    // Atomic subscribe so any subsequent fanout sees our receiver
    // attached (no zero-subscriber send window).
    let mut rx = app.broadcaster_subscribe(session_id).await;

    // Spawned fanout (mints inside the spawned task), then an inline
    // synchronous broadcast (mints then sends with nothing between).
    app.spawn_fanout(
        session_id,
        SsePayload::AssistantTokenDelta { text: "a".into() },
    );
    app.broadcast(
        session_id,
        SsePayload::AssistantTokenDelta { text: "b".into() },
    )
    .await;

    let first = tokio::time::timeout(Duration::from_secs(2), rx.recv())
        .await
        .expect("no timeout on first")
        .expect("first event");
    let second = tokio::time::timeout(Duration::from_secs(2), rx.recv())
        .await
        .expect("no timeout on second")
        .expect("second event");

    assert!(
        first.seq < second.seq,
        "seq must be monotonic in delivery order: got {} then {}",
        first.seq,
        second.seq
    );
    // Seqs are minted at send time and contiguous for this session: the
    // first two sends are 1 then 2 in delivery order.
    assert_eq!(first.seq, 1, "first delivered event must carry seq 1");
    assert_eq!(second.seq, 2, "second delivered event must carry seq 2");
}

/// Run the rapid-fanout-then-broadcast race many times; with mint-at-send
/// the delivery order is always seq-monotonic. Under the old
/// mint-before-spawn code this loop would intermittently observe the
/// inline broadcast's higher seq delivered first.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn repeated_fanout_broadcast_races_never_invert_seq() {
    let app = build_state().await;

    for _ in 0..32 {
        let (session_id, _greeting) = app
            .conversation
            .start_session(false)
            .await
            .expect("start_session");
        let mut rx = app.broadcaster_subscribe(session_id).await;

        app.spawn_fanout(
            session_id,
            SsePayload::AssistantTokenDelta {
                text: "spawned".into(),
            },
        );
        app.broadcast(
            session_id,
            SsePayload::AssistantTokenDelta {
                text: "inline".into(),
            },
        )
        .await;

        let first = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .expect("no timeout")
            .expect("first event");
        let second = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .expect("no timeout")
            .expect("second event");

        assert!(
            first.seq < second.seq,
            "delivery order seq inversion: {} then {}",
            first.seq,
            second.seq
        );
    }
}
