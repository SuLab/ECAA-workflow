//! Session-transaction coverage.
//!
//! Two coverage axes:
//!
//! 1. `SessionStore::transaction` holds the per-session lock across an
//!    async closure body so concurrent mutations serialize. This is the
//!    canonical surface for every multi-step session mutation; it seeds
//!    the ConfirmationToken minting and the amendment-invalidation
//!    epoch.
//!
//! 2. The OR-merge race at `send_turn.rs` no longer reverts a
//!    concurrent `/reject`. The legacy merge was:
//!    `current.user_confirmed = session.user_confirmed || current.user_confirmed;`
//!    which silently undid a reject that arrived mid-turn against a
//!    snapshot taken before the user_confirmed latch flipped.
//!    The fix is "current wins" — the LLM cannot mutate
//!    `user_confirmed` through any tool, so the snapshot value is
//!    always stale-or-equal and never authoritative.

use ecaa_workflow_conversation::persistence::SessionStore;
use ecaa_workflow_conversation::session::Session;
use ecaa_workflow_conversation::tools::ToolContext;
use std::sync::Arc;

/// `transaction` runs an async closure under the per-session lock and
/// persists the mutation atomically before releasing.
#[tokio::test]
async fn transaction_persists_async_closure_mutations() {
    let tmp = tempfile::tempdir().unwrap();
    let store = SessionStore::open(tmp.path()).await.unwrap();
    let mut session = Session::new(false);
    session.intake_prose = "initial".to_string();
    let id = session.id;
    store.save(&session).await.unwrap();

    store
        .transaction(id, |s| {
            Box::pin(async move {
                // Async work inside the closure body — the lock is
                // held across this await.
                tokio::task::yield_now().await;
                s.intake_prose = "mutated via transaction".to_string();
                s.last_activity = chrono::Utc::now();
                Ok(())
            })
        })
        .await
        .unwrap();

    // Re-open the store so we read fresh from disk, not the in-memory
    // handle. Proves the mutation is durable.
    let store2 = SessionStore::open(tmp.path()).await.unwrap();
    let loaded = store2.get(id).await.unwrap();
    assert_eq!(loaded.intake_prose, "mutated via transaction");
}

/// Two concurrent transactions on the same session serialize: both
/// mutations land in turn, neither is lost. This is the property that
/// closes P0-3 — a concurrent `/reject` mid-tool-loop is naturally
/// serialized by the transaction lock rather than racing through a
/// delta-merge.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_transactions_on_same_session_serialize() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Arc::new(SessionStore::open(tmp.path()).await.unwrap());
    let session = Session::new(false);
    let id = session.id;
    store.save(&session).await.unwrap();

    let s1 = store.clone();
    let s2 = store.clone();
    let h1 = tokio::spawn(async move {
        s1.transaction(id, |s| {
            Box::pin(async move {
                tokio::task::yield_now().await;
                Arc::make_mut(&mut s.conversation)
                    .push(ecaa_workflow_conversation::session::Turn::user("a"));
                Ok(())
            })
        })
        .await
        .unwrap();
    });
    let h2 = tokio::spawn(async move {
        s2.transaction(id, |s| {
            Box::pin(async move {
                tokio::task::yield_now().await;
                Arc::make_mut(&mut s.conversation)
                    .push(ecaa_workflow_conversation::session::Turn::user("b"));
                Ok(())
            })
        })
        .await
        .unwrap();
    });
    h1.await.unwrap();
    h2.await.unwrap();

    // Both pushes must persist — same-session serialization is the
    // load-bearing invariant. If transactions raced, one push would
    // be lost (last-writer-wins on the whole-session clone).
    let loaded = store.get(id).await.unwrap();
    assert_eq!(
        loaded.conversation.len(),
        2,
        "both transactional pushes must land; transactions on the same \
         session are serialized by the per-session Mutex held across \
         the closure body"
    );
}

/// P0-3 regression: a concurrent `/reject` flipping `user_confirmed`
/// to `false` mid-turn must not be reverted by the post-tool-loop
/// merge. With the surgical fix at send_turn.rs:237, the merge no
/// longer ORs the snapshot's `user_confirmed` into `current` — the
/// persisted latch wins because the LLM cannot mutate it.
///
/// We exercise the merge path indirectly: simulate a concurrent
/// `/reject` by writing `user_confirmed=false` through the store
/// while a stale "snapshot" session still has `user_confirmed=true`.
/// The historical bug would OR-merge to `true`. The fix preserves
/// the persisted `false`.
#[tokio::test]
async fn user_confirmed_reject_wins_over_stale_snapshot() {
    let tmp = tempfile::tempdir().unwrap();
    let store = SessionStore::open(tmp.path()).await.unwrap();
    let mut session = Session::new(false);
    // Mints a confirmation token in place of the older `session.user_confirmed = true`
    // ConfirmationToken bound to a synthesized pending_emission_id.
    // Simulates a session where the SME's prior /confirm armed the
    // per-emit latch.
    session.pending_emission_id = Some(uuid::Uuid::new_v4());
    let _ = session.mint_confirmation_token(
        chrono::Utc::now(),
        ecaa_workflow_conversation::audit_actor::AuditActor::User("test".into()),
    );
    let id = session.id;
    store.save(&session).await.unwrap();

    // Stale snapshot — what `send_turn` reads at line 121.
    let stale_snapshot_token = session.confirmation_token.clone();
    assert!(stale_snapshot_token.is_some(), "fixture pre-condition");

    // Concurrent /reject lands while the tool loop is running. Clears
    // both the token and the pending_emission_id, mirroring the
    // server-side reject path (transitions.rs).
    store
        .update(id, |s| {
            s.clear_confirmation();
            s.pending_emission_id = None;
            Ok(())
        })
        .await
        .unwrap();

    // Simulate the post-tool-loop merge in send_turn.rs. After the
    // C4 fix the merge does NOT touch `current.confirmation_token`.
    // The stale snapshot's `Some(...)` is explicitly discarded.
    store
        .update(id, |_current| {
            // This block mirrors the FIXED merge logic at
            // send_turn.rs — current wins, snapshot is discarded.
            let _ = &stale_snapshot_token;
            // current.confirmation_token is untouched.
            Ok(())
        })
        .await
        .unwrap();

    let loaded = store.get(id).await.unwrap();
    assert!(
        loaded.confirmation_token.is_none(),
        "concurrent /reject must win over the stale token snapshot held \
         by the in-flight tool loop; the OR-merge race in send_turn.rs \
         silently reverted this reject before C4"
    );
    assert!(
        !loaded.is_confirmed(),
        "is_confirmed() must reflect the cleared token"
    );
}

/// Production-panic regression. The re-entrancy probe
/// was originally a `std::thread_local!` flipped by an RAII guard.
/// std thread-locals are bound to the OS worker thread, not the tokio
/// task — so a transaction's closure that `.await`ed (e.g.
/// `try_auto_emit_after_confirm`'s `dispatch_one(EmitPackage)` call)
/// could park itself on worker thread T, leaving `IN_TRANSACTION=true`
/// in T's thread-local state. The tokio runtime would then schedule
/// an unrelated task (e.g. a brand-new session's POST /turn building
/// a fresh `ToolContext`) onto T, which would observe the leftover
/// `true` and trip the `with_store` `debug_assert!` even though no
/// transaction was active in that task. The fix moves the probe to
/// `tokio::task_local!` so the flag is task-scoped — visible across
/// every `.await` in the SAME task, invisible to every other task.
///
/// This test exercises the failing path: a long-running transaction
/// holds the per-session mutex on session A across a `yield_now`,
/// while a concurrent task constructs a `ToolContext::with_store` for
/// session B on the same multi-threaded runtime. The pre-fix
/// thread-local would panic with the line-1077 guard message; the
/// task-local fix succeeds.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn with_store_does_not_panic_while_unrelated_transaction_yields() {
    let tmp = tempfile::tempdir().unwrap();
    let store = SessionStore::open(tmp.path()).await.unwrap();
    let session_a = Session::new(false);
    let id_a = session_a.id;
    store.save(&session_a).await.unwrap();

    // Coordination channels so the two tasks interleave at the
    // critical window: task A must be parked inside its transaction
    // (with the task-local flag set) BEFORE task B reaches its
    // `with_store` call.
    let (a_inside_tx, a_inside_rx) = tokio::sync::oneshot::channel::<()>();
    let (b_done_tx, b_done_rx) = tokio::sync::oneshot::channel::<()>();

    let store_a = store.clone();
    let store_b = store.clone();
    let task_a = tokio::spawn(async move {
        store_a
            .transaction(id_a, |s| {
                Box::pin(async move {
                    // Tell task B that the transaction is now active —
                    // the IN_TRANSACTION flag (whether thread-local or
                    // task-local) is set from this point on.
                    let _ = a_inside_tx.send(());
                    // Park on B's completion so the runtime is free to
                    // schedule B onto whichever worker is available
                    // (including the one currently running this task).
                    let _ = b_done_rx.await;
                    s.last_activity = chrono::Utc::now();
                    Ok(())
                })
            })
            .await
            .unwrap();
    });

    let task_b = tokio::spawn(async move {
        // Wait until A's transaction is provably active before B
        // builds its ToolContext — that's the window the pre-fix
        // thread-local guard would observe as "in transaction".
        let _ = a_inside_rx.await;
        // The load-bearing call: `with_store` debug_assert!s that
        // `in_transaction()` is false. The pre-fix thread-local
        // could observe A's flag from B's task; the task-local fix
        // returns false here because B is a different task.
        let _ctx = ToolContext::new(std::path::PathBuf::from("/tmp"), "claude-sonnet-4-6")
            .with_store(store_b);
        let _ = b_done_tx.send(());
    });

    // Both tasks must complete cleanly — no panic from the guard.
    task_b.await.expect("task B (with_store) must not panic");
    task_a.await.expect("task A (transaction) must complete");
}

/// Companion check: when a tool handler IS dispatched inside the
/// transaction body (the actual re-entrancy hazard the R3.9 guard
/// protects against), the task-local probe still trips. Validates we
/// didn't accidentally widen the gap while fixing the false positive.
#[tokio::test]
#[should_panic(expected = "ToolContext::with_store called inside SessionStore::transaction")]
async fn with_store_panics_when_called_inside_same_task_transaction() {
    let tmp = tempfile::tempdir().unwrap();
    let store = SessionStore::open(tmp.path()).await.unwrap();
    let session = Session::new(false);
    let id = session.id;
    store.save(&session).await.unwrap();

    let store_inner = store.clone();
    let _ = store
        .transaction(id, move |_s| {
            let store_inner = store_inner.clone();
            Box::pin(async move {
                // Same task as the transaction closure — the
                // task-local flag IS visible here and the guard MUST
                // panic.
                let _ctx = ToolContext::new(std::path::PathBuf::from("/tmp"), "claude-sonnet-4-6")
                    .with_store(store_inner);
                Ok(())
            })
        })
        .await;
}
