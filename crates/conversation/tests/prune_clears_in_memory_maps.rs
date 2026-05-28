//! When `SessionStore::prune_expired_now` removes a
//! session from disk, in-memory maps keyed by `SessionId` that live on
//! `ConversationService` (`turn_locks` and `availability`) must also drop
//! their entries. Otherwise a long-running server accumulates per-session
//! mutex registry and `LlmAvailability` snapshots forever.
//!
//! The mechanism is a prune-hook callback registered by
//! `ConversationService::new`: when `prune_expired_now` deletes a session
//! file from disk and evicts it from the store's in-memory cache, it also
//! fires the hook so the service can clear its own per-session state.

use chrono::{Duration, Utc};
use ecaa_workflow_conversation::{
    ConversationService, LlmBackend, MockLlmBackend, SessionStore,
};
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

#[tokio::test]
async fn prune_removes_session_from_turn_locks_and_availability() {
    // `_env` keeps the tempdir alive (RAII via Arc<TempDir>) until the
    // test function exits. The earlier `std::mem::forget(dir)` would
    // which leaked the handle for the lifetime of the test process.
    let _env = TestEnv::new();
    let store = SessionStore::open(_env.path()).await.unwrap();

    let backend: Arc<dyn LlmBackend> = Arc::new(MockLlmBackend::new(vec![]));
    let svc = ConversationService::new(backend, store.clone(), config_dir());

    // Create a session: this seeds both `turn_locks` (lazily via
    // `session_turn_lock`) and `availability` (eagerly by `start_session`).
    let (id, _greeting) = svc.start_session(false).await.unwrap();
    // Force-create the turn-lock entry — `start_session` doesn't lazily
    // create it, but a real `send_turn` would. We exercise the accessor
    // directly so the test isolates the GC behavior, not the turn loop.
    let _lock = svc.session_turn_lock_for_test(id);

    // Sanity: both maps now have the session.
    assert!(
        svc.turn_locks_for_test().contains_key(&id),
        "turn_locks should contain the new session"
    );
    assert!(
        svc.availability_for_test().contains_key(&id),
        "availability should contain the new session"
    );

    // Make the on-disk session 31 days old so `prune_expired_now`
    // observes it past the 30-day TTL. The store re-loads from disk
    // during prune so we mutate the file in place.
    let path = store.dir().join(format!("{}.json", id));
    let bytes = tokio::fs::read(&path).await.unwrap();
    let mut value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let old = (Utc::now() - Duration::days(31)).to_rfc3339();
    value
        .as_object_mut()
        .unwrap()
        .insert("last_activity".to_string(), serde_json::json!(old));
    tokio::fs::write(&path, serde_json::to_vec_pretty(&value).unwrap())
        .await
        .unwrap();

    // Act: drive the prune sweep deterministically.
    store.prune_expired_now().await.unwrap();

    // Assert: both in-memory maps no longer contain the pruned id.
    assert!(
        !svc.turn_locks_for_test().contains_key(&id),
        "turn_locks must drop the pruned session"
    );
    assert!(
        !svc.availability_for_test().contains_key(&id),
        "availability must drop the pruned session"
    );
}
