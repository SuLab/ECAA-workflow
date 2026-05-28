//! Regression: concurrent existing-session mutations must serialize through
//! `SessionStore::update`. A direct save of an old snapshot can replace the
//! in-memory handle and lose a concurrent progress mutation.

use scripps_workflow_conversation::persistence::SessionStore;
use scripps_workflow_conversation::session::{HarnessEvent, Session};
use scripps_workflow_core::decision_log::{DecisionActor, DecisionRecord, DecisionType};
use std::sync::Arc;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_progress_update_and_decision_record_both_survive() {
    let tmp = tempfile::tempdir().expect("tmp");
    let store = Arc::new(
        SessionStore::open(tmp.path().to_path_buf())
            .await
            .expect("store open"),
    );

    let session = Session::new(false);
    let id = session.id;
    store.save(&session).await.expect("seed save");

    let progress_count = 100usize;
    let decision_count = 50usize;
    let mut handles = Vec::new();

    for i in 0..progress_count {
        let s = store.clone();
        handles.push(tokio::spawn(async move {
            s.update(id, |sess| {
                sess.harness_events.push(HarnessEvent {
                    kind: "task_started".into(),
                    task_id: format!("task-{i}"),
                    status: "running".into(),
                    detail: "started".into(),
                    remote: None,
                    timestamp: chrono::Utc::now(),
                });
                Ok(())
            })
            .await
        }));
    }

    for i in 0..decision_count {
        let s = store.clone();
        handles.push(tokio::spawn(async move {
            s.update(id, |sess| {
                sess.decisions.push(DecisionRecord::new(
                    id.to_string(),
                    DecisionType::Reject,
                    DecisionActor::Sme,
                    Some(format!("decision-{i}")),
                ));
                Ok(())
            })
            .await
        }));
    }

    for h in handles {
        h.await.expect("join").expect("update ok");
    }

    let final_session = store.get(id).await.expect("get");
    assert_eq!(
        final_session.harness_events.len(),
        progress_count,
        "all progress events must be preserved"
    );
    assert_eq!(
        final_session.decisions.len(),
        decision_count,
        "all decisions must be preserved"
    );
}
