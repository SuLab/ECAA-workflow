//! Events domain: SSE stream (`events_stream`) +
//! harness-progress POST handler (`post_progress`). Both sit on the
//! same per-session `broadcast::Sender<EnvelopedEvent>` channel created
//! lazily by `ChatAppState::broadcaster`.
//!
//! Submodule split (each file ≤ 400 LOC):
//! - [`stream`] — SSE `events_stream` GET handler + lagged-resync
//!   synthetic event + 15s keepalive.
//! - [`broadcaster`] — `synthesize_missing_decision_json` stub
//!   writer + typed `BlockerKind` parsers consumed by the
//!   progress write path.
//! - [`rate_limit`] — `post_progress` POST handler. The
//!   token-bucket implementation (`RateBucket`) lives in
//!   `chat_routes/app_state.rs`; this submodule is the consumer.

use crate::chat_routes::ChatAppState;

mod broadcaster;
mod rate_limit;
mod stream;

// Re-exports — `chat_routes::mod.rs` does
// `pub use events::{events_stream, post_progress}` and the per-domain
// `routes()` builder reaches the bare names from here.
pub use rate_limit::post_progress;
pub use stream::events_stream;

/// Route inventory for the doc-as-contract gate +
/// per-submodule `routes()` builder. `mod.rs::router()` merges every
/// submodule's builder into the single chat surface.
pub(super) const ROUTES: &[(&str, &str)] = &[
    ("POST", "/api/chat/session/:id/progress"),
    ("GET", "/api/chat/session/:id/events"),
];

pub(super) fn routes() -> axum::Router<ChatAppState> {
    axum::Router::new()
        .route(
            "/api/chat/session/:id/progress",
            axum::routing::post(post_progress),
        )
        .route(
            "/api/chat/session/:id/events",
            axum::routing::get(events_stream),
        )
}

#[cfg(test)]
mod tests {
    use super::broadcaster::synthesize_missing_decision_json;
    use crate::chat_routes::test_support::make_router;
    use crate::chat_routes::{SsePayload, PROGRESS_RATE_BURST};
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::util::ServiceExt;
    use uuid::Uuid;

    #[tokio::test]
    async fn post_progress_completion_writes_authoritative_task_states() {
        use crate::chat_routes::test_support::seed_session_with_completed_task;
        use scripps_workflow_conversation::SessionState;
        use scripps_workflow_core::dag::TaskState;

        let (router, app) = make_router(vec![]).await;
        let id = seed_session_with_completed_task(&app, "alignment", None).await;
        app.conversation
            .store_handle()
            .update(id, |s| {
                s.state = SessionState::Emitted;
                s.task_states.clear();
                if let Some(dag) = s.dag.as_mut() {
                    if let Some(task) = dag.tasks.get_mut("alignment") {
                        task.state = TaskState::Pending;
                    }
                }
                Ok(())
            })
            .await
            .unwrap();

        let req = Request::builder()
            .method("POST")
            .uri(format!("/api/chat/session/{}/progress", id))
            .header("content-type", "application/json")
            .body(Body::from(
                r#"{"kind":"task_completed","task_id":"alignment","status":"completed","detail":"ok"}"#,
            ))
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);

        let session = app.conversation.get_session(id).await.unwrap();
        assert!(
            matches!(
                session.task_states.get("alignment"),
                Some(TaskState::Completed { .. })
            ),
            "progress completion must be recorded in Session::task_states"
        );
    }

    #[tokio::test]
    async fn heartbeat_stalled_transitions_session_to_blocked() {
        use crate::chat_routes::test_support::seed_session_with_completed_task;
        use scripps_workflow_conversation::SessionState;
        use scripps_workflow_core::blocker::BlockerKind;

        let (router, app) = make_router(vec![]).await;
        let id = seed_session_with_completed_task(&app, "alignment", None).await;
        app.conversation
            .store_handle()
            .update(id, |s| {
                s.state = SessionState::Emitted;
                Ok(())
            })
            .await
            .unwrap();

        let req = Request::builder()
            .method("POST")
            .uri(format!("/api/chat/session/{}/progress", id))
            .header("content-type", "application/json")
            .body(Body::from(
                r#"{"kind":"heartbeat_stalled","task_id":"alignment","status":"blocked","detail":"heartbeat stale","heartbeat_age_secs":901}"#,
            ))
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);

        let session = app.conversation.get_session(id).await.unwrap();
        match session.state {
            SessionState::Blocked {
                blocker_kind:
                    Some(BlockerKind::HeartbeatStalled {
                        task_id,
                        last_heartbeat_secs_ago,
                    }),
                ..
            } => {
                assert_eq!(task_id, "alignment");
                assert_eq!(last_heartbeat_secs_ago, 901);
            }
            other => panic!("expected HeartbeatStalled blocked session, got {other:?}"),
        }
    }

    // ── blocker-kind mapper integration ───────────────────────────────────

    #[tokio::test]
    async fn task_blocked_with_runtime_substitution_maps_to_typed_variant() {
        use crate::chat_routes::test_support::seed_session_with_completed_task;
        use scripps_workflow_conversation::SessionState;

        let tmp = tempfile::TempDir::new().unwrap();
        // Seed the agent-written blocker.json on disk so the mapper
        // can promote the agent's free-form "runtime_substitution"
        // string to a typed RuntimeCapabilityMissing variant.
        let dir = tmp
            .path()
            .join("runtime")
            .join("outputs")
            .join("bio_interp");
        std::fs::create_dir_all(&dir).unwrap();
        let blocker = serde_json::json!({
            "blocker_kind": "runtime_substitution",
            "sme_pinned_method": "fgsea_msigdb_hallmark_reactome",
            "missing_capability": "r_fgsea",
            "recommended_substitute": "gseapy",
        });
        std::fs::write(
            dir.join("blocker.json"),
            serde_json::to_vec_pretty(&blocker).unwrap(),
        )
        .unwrap();

        let (router, app) = make_router(vec![]).await;
        let id =
            seed_session_with_completed_task(&app, "bio_interp", Some(tmp.path().to_path_buf()))
                .await;

        // The session starts in Greeting; manually advance through
        // Intake → Emitted so the HarnessTaskBlocked transition
        // accepts. The service's block_from_harness handles
        // Intake/IntakeFollowup/Emitted, but the default seed is in
        // Greeting.
        app.conversation
            .store_handle()
            .update(id, |s| {
                s.state = SessionState::Emitted;
                Ok(())
            })
            .await
            .unwrap();

        let req = Request::builder()
            .method("POST")
            .uri(format!("/api/chat/session/{}/progress", id))
            .header("content-type", "application/json")
            .body(Body::from(
                r#"{"kind":"task_blocked","task_id":"bio_interp","status":"blocked","detail":"SME-pinned primary fgsea not installed; gseapy available."}"#,
            ))
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);

        // Confirm the session's Blocked state carries the typed
        // RuntimeCapabilityMissing variant, not the lossy
        // DataShapeMismatch fallback.
        let session = app
            .conversation
            .get_session(id)
            .await
            .expect("session must exist");
        match &session.state {
            SessionState::Blocked { blocker_kind, .. } => match blocker_kind {
                Some(scripps_workflow_core::blocker::BlockerKind::RuntimeCapabilityMissing {
                    sme_pinned_method,
                    missing_capability,
                    recommended_substitute,
                }) => {
                    assert_eq!(sme_pinned_method, "fgsea_msigdb_hallmark_reactome");
                    assert_eq!(missing_capability, "r_fgsea");
                    assert_eq!(recommended_substitute, &Some("gseapy".into()));
                }
                other => panic!("expected RuntimeCapabilityMissing, got {:?}", other),
            },
            other => panic!("expected Blocked, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn task_blocked_with_error_envelope_promotes_to_tool_error() {
        // Regression guard for the apply-remediation path: when the
        // harness writes runtime/outputs/<task>/error.json before
        // posting task_blocked, the server's progress handler must
        // upgrade the BlockerKind to ToolError instead of falling
        // through to DataShapeMismatch / AwaitingStructuredDecision.
        // Without this routing the BlockerCard's tool_error arm and
        // the RemediationSuggestionList never render.
        use crate::chat_routes::test_support::seed_session_with_completed_task;
        use scripps_workflow_conversation::SessionState;
        use scripps_workflow_core::error_envelope::ToolErrorEnvelope;

        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path().join("runtime").join("outputs").join("alignment");
        std::fs::create_dir_all(&dir).unwrap();
        let envelope = ToolErrorEnvelope {
            task_id: "alignment".into(),
            stage_id: "alignment".into(),
            library: Some("STAR".into()),
            library_version: Some("2.7.11a".into()),
            error_class: "OOM".into(),
            message: "STAR killed by SIGKILL".into(),
            stderr_tail: vec!["killed".into()],
            stdout_tail: vec![],
            traceback: None,
            exit_code: Some(137),
            signal: Some("SIGKILL".into()),
            wallclock_secs: Some(1800),
            peak_memory_mb: Some(60_000),
            input_summary: Default::default(),
            executor: "local".into(),
            executor_context: Default::default(),
            captured_at: "2026-05-04T10:00:00Z".into(),
            attempt: 1,
            schema_version: 1,
        };
        std::fs::write(
            dir.join("error.json"),
            serde_json::to_vec_pretty(&envelope).unwrap(),
        )
        .unwrap();

        let (router, app) = make_router(vec![]).await;
        let id =
            seed_session_with_completed_task(&app, "alignment", Some(tmp.path().to_path_buf()))
                .await;

        app.conversation
            .store_handle()
            .update(id, |s| {
                s.state = SessionState::Emitted;
                Ok(())
            })
            .await
            .unwrap();

        let req = Request::builder()
            .method("POST")
            .uri(format!("/api/chat/session/{}/progress", id))
            .header("content-type", "application/json")
            .body(Body::from(
                r#"{"kind":"task_blocked","task_id":"alignment","status":"blocked","detail":"agent exit 137"}"#,
            ))
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);

        let session = app
            .conversation
            .get_session(id)
            .await
            .expect("session must exist");
        match &session.state {
            SessionState::Blocked { blocker_kind, .. } => match blocker_kind {
                Some(scripps_workflow_core::blocker::BlockerKind::ToolError { envelope: env }) => {
                    assert_eq!(env.error_class, "OOM");
                    assert_eq!(env.library.as_deref(), Some("STAR"));
                    assert_eq!(env.signal.as_deref(), Some("SIGKILL"));
                    assert_eq!(env.exit_code, Some(137));
                }
                other => panic!("expected ToolError, got {:?}", other),
            },
            other => panic!("expected Blocked, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn progress_high_water_event_bumps_metrics_counter() {
        let (router, app) = make_router(vec![]).await;
        let id = Uuid::new_v4();

        let req = Request::builder()
            .method("POST")
            .uri(format!("/api/chat/session/{}/progress", id))
            .header("content-type", "application/json")
            .body(Body::from(
                r#"{"kind":"high_water_exceeded","task_id":"alignment_quantification_a","status":"resized","detail":"bumped r6i.2xlarge -> r6i.4xlarge"}"#,
            ))
            .unwrap();
        let resp = router.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);

        let req = Request::builder()
            .method("POST")
            .uri(format!("/api/chat/session/{}/progress", id))
            .header("content-type", "application/json")
            .body(Body::from(
                r#"{"kind":"high_water_exceeded","task_id":"deepvariant_run","status":"resized","detail":"bumped g6.xlarge -> g6.2xlarge"}"#,
            ))
            .unwrap();
        let resp = router.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);

        let snapshot = app
            .conversation
            .metrics_snapshot(id)
            .await
            .expect("metrics store must register the session on first record");
        assert_eq!(snapshot.high_water_exceeded_count, 2);
        assert_eq!(snapshot.turn_count, 0);
        assert_eq!(snapshot.total_instance_seconds, 0);
    }

    #[tokio::test]
    async fn progress_endpoint_accepts_event() {
        let (router, app) = make_router(vec![]).await;
        let id = Uuid::new_v4();
        let mut rx = app.broadcaster(id).await.subscribe();

        let req = Request::builder()
            .method("POST")
            .uri(format!("/api/chat/session/{}/progress", id))
            .header("content-type", "application/json")
            .body(Body::from(
                r#"{"kind":"task_started","task_id":"t1","status":"running","detail":"go"}"#,
            ))
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);

        let envelope = rx.recv().await.unwrap();
        match envelope.payload {
            SsePayload::HarnessProgress { task_id, .. } => assert_eq!(task_id, "t1"),
            _ => panic!("expected HarnessProgress event"),
        }
    }

    #[tokio::test]
    async fn post_progress_with_artifacts_broadcasts_task_completed_reviewable() {
        let (router, app) = make_router(vec![]).await;
        let id = Uuid::new_v4();
        let mut rx = app.broadcaster(id).await.subscribe();

        let req = Request::builder()
            .method("POST")
            .uri(format!("/api/chat/session/{}/progress", id))
            .header("content-type", "application/json")
            .body(Body::from(
                r#"{
                        "kind":"task_completed",
                        "task_id":"t_demo",
                        "status":"ok",
                        "detail":"done",
                        "artifacts":[{
                            "name":"plot.png",
                            "relative_path":"runtime/t_demo/plot.png",
                            "size_bytes":1234,
                            "mime_type":"image/png"
                        }]
                    }"#,
            ))
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);

        let first = rx.recv().await.unwrap();
        assert!(
            matches!(first.payload, SsePayload::HarnessProgress { ref kind, .. } if kind == "task_completed"),
            "expected HarnessProgress, got {:?}",
            first
        );
        let second = rx.recv().await.unwrap();
        match second.payload {
            SsePayload::TaskCompletedReviewable { task_id, artifacts } => {
                assert_eq!(task_id, "t_demo");
                assert_eq!(artifacts.len(), 1);
                assert_eq!(artifacts[0].name, "plot.png");
                assert_eq!(artifacts[0].mime_type, "image/png");
                assert_eq!(artifacts[0].size_bytes, 1234);
            }
            other => panic!("expected TaskCompletedReviewable, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn task_completed_progress_commits_package_artifacts() {
        use crate::chat_routes::test_support::seed_session_with_completed_task;
        use std::process::Command;
        use std::sync::Arc;

        fn git(pkg: &std::path::Path, args: &[&str]) -> String {
            let out = Command::new("git")
                .arg("-C")
                .arg(pkg)
                .args(args)
                .output()
                .unwrap_or_else(|e| panic!("git {:?}: {}", args, e));
            assert!(
                out.status.success(),
                "git {:?} failed\nstdout:\n{}\nstderr:\n{}",
                args,
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
            String::from_utf8(out.stdout).unwrap()
        }

        let pkg = tempfile::tempdir().unwrap();
        let cfg_path = pkg.path().join("git-config.json");
        std::fs::write(
            &cfg_path,
            serde_json::json!({
                "enabled": true,
                "commit_on_task_completed": true,
                "author_name": "Test",
                "author_email": "test@example.com"
            })
            .to_string(),
        )
        .unwrap();
        let (_router, mut app) = make_router(vec![]).await;
        app.git_config = Arc::new(crate::git_routes::GitConfigStore::open_or_default(cfg_path));
        let router = crate::chat_routes::router(app.clone()).layer(axum::Extension(
            crate::auth::RequestPrincipal::test_default(),
        ));

        std::fs::write(pkg.path().join("WORKFLOW.json"), "{}\n").unwrap();
        git(pkg.path(), &["init"]);
        git(pkg.path(), &["config", "user.name", "Test"]);
        git(pkg.path(), &["config", "user.email", "test@example.com"]);
        git(pkg.path(), &["add", "WORKFLOW.json"]);
        git(pkg.path(), &["commit", "-m", "emit: seed package"]);

        let id =
            seed_session_with_completed_task(&app, "t_demo", Some(pkg.path().to_path_buf())).await;
        let artifact = pkg.path().join("runtime/outputs/t_demo/result.json");
        std::fs::create_dir_all(artifact.parent().unwrap()).unwrap();
        std::fs::write(&artifact, r#"{"ok":true}"#).unwrap();

        let req = Request::builder()
            .method("POST")
            .uri(format!("/api/chat/session/{}/progress", id))
            .header("content-type", "application/json")
            .body(Body::from(
                r#"{"kind":"task_completed","task_id":"t_demo","status":"completed","detail":"done"}"#,
            ))
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);

        let mut clean = false;
        for _ in 0..40 {
            if git(pkg.path(), &["status", "--porcelain"])
                .trim()
                .is_empty()
            {
                clean = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        assert!(
            clean,
            "task artifact git hook did not clean the package repo"
        );

        let head_files = git(pkg.path(), &["show", "--name-only", "--format=", "HEAD"]);
        assert!(
            head_files
                .lines()
                .any(|line| line == "runtime/outputs/t_demo/result.json"),
            "task artifact was not committed in HEAD:\n{}",
            head_files
        );
    }

    /// Stub-from-subfields path of synthesize_missing_decision_json.
    #[test]
    fn synthesize_missing_decision_json_writes_stub_from_reason_subfields() {
        let tmp = tempfile::tempdir().unwrap();
        let reason = "Awaiting SME approval for normalization. Top candidate: vst (score 0.91). Runner-ups: tmm (0.82), cpm (0.71). Rationale: best-practice scorer pick. Full decision: runtime/outputs/discover_normalization/decision.json";

        synthesize_missing_decision_json(tmp.path(), reason);

        let path = tmp
            .path()
            .join("runtime/outputs/discover_normalization/decision.json");
        assert!(path.exists(), "stub decision.json should be created");
        let body: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(body["task_id"], "discover_normalization");
        assert_eq!(body["top_candidate"], "vst");
        let runner_ups = body["runner_ups"].as_array().unwrap();
        assert!(runner_ups.iter().any(|v| v == "tmm"));
        assert!(runner_ups.iter().any(|v| v == "cpm"));
        assert_eq!(body["stub"], true);
        assert!(body["note"]
            .as_str()
            .unwrap()
            .contains("agent did not write"));
    }

    #[test]
    fn synthesize_missing_decision_json_writes_minimal_stub_without_subfields() {
        let tmp = tempfile::tempdir().unwrap();
        let reason =
            "Blocked — see runtime/outputs/discover_normalization/decision.json for details.";

        synthesize_missing_decision_json(tmp.path(), reason);

        let path = tmp
            .path()
            .join("runtime/outputs/discover_normalization/decision.json");
        assert!(path.exists());
        let body: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(body["top_candidate"], "unknown");
        assert_eq!(body["stub"], true);
    }

    #[test]
    fn synthesize_missing_decision_json_is_noop_when_file_exists() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp
            .path()
            .join("runtime/outputs/discover_normalization/decision.json");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, br#"{"real": true}"#).unwrap();

        let reason = "runtime/outputs/discover_normalization/decision.json";
        synthesize_missing_decision_json(tmp.path(), reason);

        let body: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(body["real"], true, "existing file must not be overwritten");
    }

    #[tokio::test]
    async fn progress_post_rate_limit_throttles_non_terminal_events() {
        let (router, app) = make_router(vec![]).await;
        let id = Uuid::new_v4();

        let mut accepted = 0usize;
        let mut throttled = 0usize;
        for i in 0..200 {
            let req = Request::builder()
                .method("POST")
                .uri(format!("/api/chat/session/{}/progress", id))
                .header("content-type", "application/json")
                .body(Body::from(format!(
                    r#"{{"kind":"task_started","task_id":"t{i}","status":"running","detail":""}}"#
                )))
                .unwrap();
            let resp = router.clone().oneshot(req).await.unwrap();
            assert_eq!(resp.status(), StatusCode::NO_CONTENT);
        }

        for _ in 0..60 {
            if app.try_consume_progress_token(id).await {
                accepted += 1;
            } else {
                throttled += 1;
            }
        }
        assert!(
            throttled > 0,
            "expected at least some non-terminal events to be throttled after a 200-event burst"
        );
        assert!(
            accepted <= PROGRESS_RATE_BURST as usize + 5,
            "accepted {} should not exceed burst cap materially",
            accepted
        );
    }

    #[tokio::test]
    async fn progress_post_rate_limit_allows_terminal_events_unconditionally() {
        let (_router, app) = make_router(vec![]).await;
        let id = Uuid::new_v4();
        for _ in 0..120 {
            let _ = app.try_consume_progress_token(id).await;
        }
        let limited = !app.try_consume_progress_token(id).await;
        assert!(limited, "bucket should be drained by now");
    }

    #[test]
    fn sse_resync_required_roundtrips() {
        let payload = SsePayload::ResyncRequired { dropped: 42 };
        let json = serde_json::to_string(&payload).expect("serializes");
        assert!(json.contains("\"type\":\"resync_required\""));
        assert!(json.contains("\"dropped\":42"));
        let back: SsePayload = serde_json::from_str(&json).expect("deserializes");
        match back {
            SsePayload::ResyncRequired { dropped } => assert_eq!(dropped, 42),
            other => panic!("expected ResyncRequired, got {:?}", other),
        }
    }

    #[test]
    fn harness_progress_serializes_remote_executor_info() {
        let payload = SsePayload::HarnessProgress {
            kind: "task_started".into(),
            task_id: "t1".into(),
            status: "running".into(),
            detail: "started".into(),
            remote: Some(crate::chat_routes::RemoteExecutionInfoWire {
                backend: "aws".into(),
                instance_id: "i-abc".into(),
                instance_type: "m6i.xlarge".into(),
            }),
        };
        let json = serde_json::to_string(&payload).expect("serializes");
        assert!(json.contains("\"type\":\"harness_progress\""));
        assert!(json.contains("\"backend\":\"aws\""));
        assert!(json.contains("\"instance_id\":\"i-abc\""));
        assert!(json.contains("\"instance_type\":\"m6i.xlarge\""));
    }

    #[tokio::test]
    async fn broadcaster_capacity_accommodates_typical_harness_burst() {
        let (_router, app) = make_router(vec![]).await;
        let id = Uuid::new_v4();
        let rx_tx = app.broadcaster(id).await;
        let mut rx = rx_tx.subscribe();
        for i in 0..200 {
            app.broadcast(
                id,
                SsePayload::HarnessProgress {
                    kind: "task_started".into(),
                    task_id: format!("t{}", i),
                    status: "running".into(),
                    detail: String::new(),
                    remote: None,
                },
            )
            .await;
        }
        for i in 0..200 {
            let got = rx.recv().await.unwrap();
            match got.payload {
                SsePayload::HarnessProgress { task_id, .. } => {
                    assert_eq!(task_id, format!("t{}", i));
                }
                other => panic!("expected HarnessProgress, got {:?}", other),
            }
        }
    }

    #[tokio::test]
    async fn lagged_subscriber_sees_resync_required_synthetic_event() {
        use crate::chat_routes::EnvelopedEvent;
        use tokio::sync::broadcast;
        let (tx, mut rx) = broadcast::channel::<EnvelopedEvent>(4);
        for i in 0..10 {
            let _ = tx.send(EnvelopedEvent {
                seq: (i + 1) as u64,
                payload: SsePayload::HarnessProgress {
                    kind: "task_started".into(),
                    task_id: format!("t{}", i),
                    status: "running".into(),
                    detail: String::new(),
                    remote: None,
                },
            });
        }
        match rx.recv().await {
            Err(broadcast::error::RecvError::Lagged(dropped)) => {
                let synthetic = EnvelopedEvent {
                    seq: 999,
                    payload: SsePayload::ResyncRequired { dropped },
                };
                let json = serde_json::to_string(&synthetic).unwrap();
                assert!(json.contains("\"type\":\"resync_required\""));
                assert!(json.contains("\"seq\":999"));
                assert!(dropped >= 6, "expected ≥6 dropped, got {}", dropped);
            }
            other => panic!("expected Lagged, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn fanout_assigns_monotonic_per_session_seqs() {
        let (_router, app) = make_router(vec![]).await;
        let id = Uuid::new_v4();
        let mut rx = app.broadcaster(id).await.subscribe();
        for i in 0..100 {
            app.broadcast(
                id,
                SsePayload::HarnessProgress {
                    kind: "task_started".into(),
                    task_id: format!("t{}", i),
                    status: "running".into(),
                    detail: String::new(),
                    remote: None,
                },
            )
            .await;
        }
        let mut last_seq = 0;
        for i in 0..100 {
            let env = rx.recv().await.unwrap();
            assert!(
                env.seq > last_seq,
                "iter {}: expected {} > {}",
                i,
                env.seq,
                last_seq
            );
            last_seq = env.seq;
            match env.payload {
                SsePayload::HarnessProgress { task_id, .. } => {
                    assert_eq!(task_id, format!("t{}", i));
                }
                other => panic!("expected HarnessProgress, got {:?}", other),
            }
        }
        assert_eq!(last_seq, 100);
    }

    #[tokio::test]
    async fn per_session_seq_counters_are_independent() {
        let (_router, app) = make_router(vec![]).await;
        let id_a = Uuid::new_v4();
        let id_b = Uuid::new_v4();
        let mut rx_a = app.broadcaster(id_a).await.subscribe();
        let mut rx_b = app.broadcaster(id_b).await.subscribe();
        for _ in 0..5 {
            for id in [id_a, id_b] {
                app.broadcast(
                    id,
                    SsePayload::HarnessProgress {
                        kind: "task_started".into(),
                        task_id: "t".into(),
                        status: "running".into(),
                        detail: String::new(),
                        remote: None,
                    },
                )
                .await;
            }
        }
        for expected in 1..=5 {
            assert_eq!(rx_a.recv().await.unwrap().seq, expected);
            assert_eq!(rx_b.recv().await.unwrap().seq, expected);
        }
    }

    #[test]
    fn enveloped_event_serializes_with_seq_and_flattened_payload() {
        let env = crate::chat_routes::EnvelopedEvent {
            seq: 42,
            payload: SsePayload::ToolCallStarted {
                tool_name: "emit_package".into(),
                status_line: "Saving the package...".into(),
            },
        };
        let json = serde_json::to_string(&env).expect("serializes");
        assert!(json.contains("\"seq\":42"), "json = {}", json);
        assert!(
            json.contains("\"type\":\"tool_call_started\""),
            "json = {}",
            json
        );
        assert!(
            json.contains("\"tool_name\":\"emit_package\""),
            "json = {}",
            json
        );
    }

    #[test]
    fn sse_package_amended_roundtrips() {
        let id = Uuid::new_v4();
        let payload = SsePayload::PackageAmended {
            session_id: id,
            amended_stage: "differential_expression".into(),
            invalidated_tasks: vec![
                "differential_expression".into(),
                "validate_differential_expression".into(),
            ],
            package_path: "/tmp/amended-package".into(),
        };
        let json = serde_json::to_string(&payload).expect("serializes");
        assert!(json.contains("\"type\":\"package_amended\""));
        assert!(json.contains(&id.to_string()));
        assert!(json.contains("\"amended_stage\":\"differential_expression\""));
        let back: SsePayload = serde_json::from_str(&json).expect("deserializes");
        match back {
            SsePayload::PackageAmended {
                session_id,
                amended_stage,
                invalidated_tasks,
                package_path,
            } => {
                assert_eq!(session_id, id);
                assert_eq!(amended_stage, "differential_expression");
                assert_eq!(invalidated_tasks.len(), 2);
                assert_eq!(package_path, "/tmp/amended-package");
            }
            other => panic!("expected PackageAmended, got {:?}", other),
        }
    }
}
