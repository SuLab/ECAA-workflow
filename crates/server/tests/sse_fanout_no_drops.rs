//! RC-19 regression test for SSE fanout liveness. Validates the
//! end-to-end fanout success counter under burst load: N fanouts →
//! `sse_sent_count` reaches N once the spawned tasks drain. A
//! `try_read` on the broadcasters map would silently fail whenever a
//! parallel writer held the write lock (e.g. `broadcaster()` lazy-init
//! for a fresh subscriber); token deltas in particular cannot recover
//! from drops because there is no resync signal. The DashMap-backed
//! map has no global write-lock window, so the bound this test
//! enforces is "no drops under contention".

use ecaa_workflow_conversation::{LlmBackend, MockLlmBackend, SessionStore};
use ecaa_workflow_server::chat_routes::{ChatAppState, SsePayload};
use std::path::PathBuf;
use std::sync::atomic::Ordering;
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

/// Under burst fanout load every spawned task must eventually deliver to
/// the broadcast channel; `sse_sent_count` reaches N once the spawned
/// fanout tasks have drained. With the prior `try_read` implementation
/// concurrent fanouts under broadcasters write-lock contention silently
/// dropped; after the DashMap migration there is no global write-lock,
/// so this regression now exercises the steady-state burst path.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sse_fanout_does_not_drop_under_write_lock_contention() {
    let app = build_state().await;
    let (session_id, _greeting) = app
        .conversation
        .start_session(false)
        .await
        .expect("start_session");

    // Seed the broadcasters map so fanout has a sender to deliver to.
    // Hold a subscriber so `tx.send` returns Ok (broadcast::send returns
    // Err when there are zero active receivers — that path would skip the
    // counter even with read().await).
    let _rx = {
        let entry = app
            .broadcasters
            .entry(session_id)
            .or_insert_with(|| tokio::sync::broadcast::channel(256).0);
        entry.subscribe()
    };

    // Baseline.
    assert_eq!(app.sse_sent_count(), 0);

    let n_events: u64 = 64;
    let mut handles = Vec::with_capacity(n_events as usize);
    for i in 0..n_events {
        let app = app.clone();
        let payload = SsePayload::AssistantTokenDelta {
            text: format!("delta-{}", i),
        };
        handles.push(tokio::spawn(async move {
            app.spawn_fanout(session_id, payload);
        }));
    }
    for h in handles {
        let _ = h.await;
    }

    // Wait for spawned fanouts to drain.
    tokio::time::sleep(Duration::from_millis(200)).await;

    let sent = app.sse_sent_count.load(Ordering::Relaxed);
    assert!(
        sent >= n_events,
        "fanout dropped {} of {} events",
        n_events.saturating_sub(sent),
        n_events,
    );
}
