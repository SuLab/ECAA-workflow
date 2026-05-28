//! `post_progress` (POST `/api/chat/session/:id/progress`)
//! handler. Drives the harness → server progress wire: fold remote
//! task wallclock into metrics, sync DAG task-state, transition the
//! session state machine on `task_blocked`, and broadcast SSE
//! `HarnessProgress` + secondary payloads (sizing-pilot, executor-
//! selected, orphan-reap, dispositions, …).
//!
//! The token-bucket implementation that enforces non-terminal POST
//! cadence (`PROGRESS_RATE_PER_SEC` / `PROGRESS_RATE_BURST` /
//! `RateBucket`) lives in `chat_routes/app_state.rs`. Terminal events
//! (`task_completed` / `task_failed` / `task_blocked` /
//! `heartbeat_stalled` / `high_water_exceeded`) bypass the rate limiter so
//! state-transitioning events are never dropped.

use super::broadcaster::{parse_harness_blocker_kind_with_file, synthesize_missing_decision_json};
use crate::chat_routes::{
    AgentUsageWire, ChatAppState, DropNotifier, HarnessProgressEvent, SsePayload,
};
use axum::{
    extract::{Path, State},
    http::{HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use ecaa_workflow_conversation::HarnessEvent;
use std::sync::Arc;
use uuid::Uuid;

fn is_zero_token_fixture_usage(usage: &AgentUsageWire) -> bool {
    usage.model == "fixture-plots"
        && usage.input_tokens == 0
        && usage.output_tokens == 0
        && usage.cache_read_tokens == 0
        && usage.cache_creation_tokens == 0
}

/// `POST /api/chat/session/:id/progress` — ingest a harness lifecycle event, advance DAG state, and broadcast SSE.
pub async fn post_progress(
    State(app): State<ChatAppState>,
    Path(session_id): Path<Uuid>,
    Json(event): Json<HarnessProgressEvent>,
) -> Response {
    // Demoted from `eprintln!` so
    // session-id correlation lives in the structured tracing pipeline
    // rather than the raw stderr stream. `?session_id` borrows the
    // UUID's Debug impl so the field is captured as a key=value pair
    // by the TraceLayer subscriber.
    tracing::debug!(
        ?session_id,
        kind = %event.kind,
        task_id = %event.task_id,
        "post_progress"
    );
    // When the event reports a remote
    // executor, accumulate per-task instance_seconds in the metrics
    // store. task_started opens an interval; task_completed closes
    // it. Other event kinds skip the metrics path.
    let now_ms = chrono::Utc::now().timestamp_millis().max(0) as u64;
    if let Some(remote) = &event.remote {
        match event.kind.as_str() {
            "task_started" => {
                app.conversation
                    .metrics()
                    .record_task_started(session_id, &event.task_id, &remote.instance_type, now_ms)
                    .await;
            }
            "task_completed" | "task_failed" | "task_blocked" | "heartbeat_stalled" => {
                app.conversation
                    .metrics()
                    .record_task_completed(session_id, &event.task_id, now_ms)
                    .await;
            }
            _ => {}
        }
    } else {
        // Local-executor path: track wall-clock per task even though
        // there's no instance_type to bucket. This is what feeds the
        // UI's ETA computation in `etaFromHistory` — without it, local
        // sessions never get per-task durations.
        match event.kind.as_str() {
            "task_started" => {
                app.conversation
                    .metrics()
                    .record_task_started_local(session_id, &event.task_id, now_ms)
                    .await;
            }
            "task_completed" | "task_failed" | "task_blocked" | "heartbeat_stalled" => {
                app.conversation
                    .metrics()
                    .record_task_completed(session_id, &event.task_id, now_ms)
                    .await;
            }
            _ => {}
        }
    }

    // Bundle D: when the agent wrote runtime/outputs/<tid>/agent-usage.json
    // the harness parses it and forwards the usage block on the
    // `task_completed` event. Fold it into the session's agent-side
    // spend counters so `/api/chat/session/:id/metrics` surfaces
    // `agent_cost_usd` alongside the chat `total_cost_usd`. No-op
    // when the field is absent (older harness / no agent
    // instrumentation).
    if event.kind == "task_completed" {
        if let Some(usage) = &event.agent_usage {
            if !is_zero_token_fixture_usage(usage) {
                // F1 — pass through task_id so the metrics store can build
                // a per-task agent cost breakdown for the UI. The wire
                // shape's task_id is required; an empty string is treated
                // as "no task tag" so the legacy aggregate-only path still
                // works for older harness builds / mock fixtures.
                let task_id_opt: Option<&str> = if event.task_id.is_empty() {
                    None
                } else {
                    Some(event.task_id.as_str())
                };
                app.conversation
                    .metrics()
                    .record_agent_usage_for_task(
                        session_id,
                        task_id_opt,
                        &usage.model,
                        usage.input_tokens,
                        usage.output_tokens,
                        usage.cache_read_tokens,
                        usage.cache_creation_tokens,
                    )
                    .await;
            }
        }
    }

    // The harness emits an event with `kind == "high_water_exceeded"`
    // when ECAA_AWS_HIGH_WATER_POLICY=resize bumps an instance
    // type above its sizing baseline. The metrics store already has
    // the counter field; this wire-up lets the SSE event increment it
    // without requiring another server change. The event has no
    // remote envelope requirement — the kind alone is the signal.
    if event.kind == "high_water_exceeded" {
        app.conversation
            .metrics()
            .record_high_water_exceeded(session_id)
            .await;
    }

    // rate-limit the persistence path for non-terminal events.
    // A runaway agent firing 1000 task_started events/sec amplified
    // into 1000 store.update() calls (and their serialize+disk-write
    // hold-time) before the batcher's 10s window could coalesce them.
    // Terminal events (task_completed/failed/blocked/heartbeat_stalled,
    // plus high_water_exceeded) always get a slot because they carry state
    // transitions the UI and session state-machine can't afford to
    // miss. The batcher enqueue below still runs regardless, so even
    // rate-limited events eventually surface in the synthetic turn.
    let is_terminal = matches!(
        event.kind.as_str(),
        "task_completed"
            | "task_failed"
            | "task_blocked"
            | "heartbeat_stalled"
            | "high_water_exceeded"
    );
    let rate_limited = if is_terminal {
        // Still consume a token to keep the bucket honest, but never
        // block on terminal events.
        let _ = app.try_consume_progress_token(session_id).await;
        false
    } else {
        !app.try_consume_progress_token(session_id).await
    };

    // Sync harness task-state changes into the session DAG + transition
    // session state on task_blocked, in a SINGLE atomic store.update so
    // parallel progress events can't clobber each other; a read-modify-
    // write split would let a concurrent `task_completed` arriving
    // milliseconds later revert an Emitted→Blocked transition.
    let mut broadcast_state_advance = false;
    if !rate_limited && (!event.task_id.is_empty() || event.kind == "task_blocked") {
        let kind = event.kind.clone();
        let task_id = event.task_id.clone();
        let detail = event.detail.clone();
        // pre-read the session's emitted
        // package path so we can upgrade the blocker kind via the
        // typed mapper (reads runtime/outputs/<task>/blocker.json and
        // maps runtime_substitution / awaiting_sme_input / etc. into
        // first-class BlockerKind variants). Falls back to the legacy
        // parser when the session has no package or the file is
        // missing. See `parse_harness_blocker_kind_with_file`.
        let package_dir = app
            .conversation
            .get_session(session_id)
            .await
            .and_then(|s| s.emitted_package_path.clone());
        let blocker_kind = if event.kind == "heartbeat_stalled" {
            ecaa_workflow_core::blocker::BlockerKind::HeartbeatStalled {
                task_id: event.task_id.clone(),
                last_heartbeat_secs_ago: event.heartbeat_age_secs.unwrap_or(0),
            }
        } else {
            parse_harness_blocker_kind_with_file(
                &event.detail,
                package_dir.as_deref(),
                &event.task_id,
            )
        };
        // Capture pre-state so we can detect ANY state-kind change
        // (not just Emitted → Blocked). After R-3, the symmetric
        // un-block transition (Blocked → Emitted on terminal task
        // events) needs to broadcast state_advanced so the client's
        // NeedsInputChip and other state-dependent UI clear without
        // waiting for a transcript poll.
        let pre_state_kind: Option<String> = app
            .conversation
            .get_session(session_id)
            .await
            .map(|s| format!("{:?}", std::mem::discriminant(&s.state)));
        let store = app.conversation.store_handle();
        let remote_state =
            event
                .remote
                .as_ref()
                .map(|r| ecaa_workflow_core::dag::RemoteExecution {
                    backend: r.backend.clone(),
                    instance_id: r.instance_id.clone(),
                    instance_type: r.instance_type.clone(),
                    command_id: None,
                    output_uri: None,
                });
        let heartbeat_age_secs = event.heartbeat_age_secs.unwrap_or(0);
        let result = store
            .update(session_id, move |s| {
                use ecaa_workflow_core::dag::{BlockedRecord, TaskState};
                // Sync task state into Session::task_states (the
                // authoritative map) and the eager DAG cache through the
                // session API. Directly mutating `s.dag` loses the state
                // after cache invalidation.
                if !task_id.is_empty() {
                    let new_state = match kind.as_str() {
                        "task_started" => Some(TaskState::Running {
                            started_at: ecaa_workflow_core::time_helpers::now_rfc3339(),
                            remote: remote_state.clone(),
                        }),
                        "task_completed" => Some(TaskState::Completed {
                            result: serde_json::json!({ "detail": detail.clone() }),
                        }),
                        "task_failed" => Some(TaskState::Failed {
                            reason: detail.clone(),
                        }),
                        "task_blocked" | "heartbeat_stalled" => Some(TaskState::Blocked {
                            record: BlockedRecord {
                                reason: detail.clone(),
                                attempts: vec![],
                            },
                        }),
                        _ => None,
                    };
                    if let Some(state) = new_state {
                        s.set_task_state(&task_id, state);
                    }
                }
                // Transition session state on task_blocked / heartbeat_stalled (same txn
                // so the blocker isn't lost to a concurrent save).
                if kind == "task_blocked" || kind == "heartbeat_stalled" {
                    eprintln!(
                        "[closure] {} for {} — session state is {:?}",
                        kind, task_id, s.state
                    );
                    // The state machine table accepts HarnessTaskBlocked
                    // from Emitted | ReadyToEmit | Amending — gating on
                    // Emitted alone would silently swallow every blocker
                    // after an amend transitioned the session out of
                    // Emitted. try_transition returns TransitionError
                    // for truly-illegal sources (Greeting/Intake/etc.)
                    // so it's safe to always try.
                    match s.try_transition(
                        ecaa_workflow_conversation::session::StateTrigger::HarnessTaskBlocked {
                            task_id: task_id.clone(),
                            detail: if kind == "heartbeat_stalled" && detail.is_empty() {
                                format!("heartbeat stale for {} seconds", heartbeat_age_secs)
                            } else {
                                detail.clone()
                            },
                            blocker_kind: blocker_kind.clone(),
                        },
                    ) {
                        Ok(()) => eprintln!("[closure] transition OK, new state = {:?}", s.state),
                        Err(e) => eprintln!("[closure] transition FAILED: {}", e),
                    }
                }
                Ok(())
            })
            .await;
        if let Ok(saved) = result {
            // Broadcast on any actual state-kind change. Firing only on
            // Emitted → Blocked would leave the client stale on the
            // symmetric Blocked → Emitted transition (the NeedsInputChip
            // stays at "1 step needs input" after the SME accepts via
            // BlockerCard). Any flip in state discriminant fires the
            // broadcast — including pure-task state changes that don't
            // change session state (those are broadcast as no-ops, which
            // is fine: the client refetches /state and the response
            // is unchanged).
            let post_state_kind = format!("{:?}", std::mem::discriminant(&saved.state));
            let state_changed = pre_state_kind
                .as_deref()
                .map(|p| p != post_state_kind)
                .unwrap_or(false);
            if state_changed {
                broadcast_state_advance = true;
            } else if matches!(event.kind.as_str(), "task_blocked" | "heartbeat_stalled")
                && matches!(
                    saved.state,
                    ecaa_workflow_conversation::SessionState::Blocked { .. }
                )
            {
                // Belt-and-suspenders: keep the original guard for
                // the (rare) case where pre_state_kind capture
                // failed (e.g., session not found between read and
                // write — shouldn't happen but the get_session is
                // technically racy with deletes).
                broadcast_state_advance = true;
            }

            if event.kind == "task_completed" && !event.task_id.is_empty() {
                if let Some(pkg) = package_dir.clone() {
                    let cfg = app.git_config().read().clone();
                    let sid_str = session_id.to_string();
                    let task_id = event.task_id.clone();
                    let app_for_drop = app.clone();
                    let drop_notifier: DropNotifier =
                        Arc::new(move |trigger: &str, reason: &str| {
                            app_for_drop.spawn_fanout(
                                session_id,
                                SsePayload::ProvenanceCommitDropped {
                                    trigger: trigger.to_string(),
                                    reason: reason.to_string(),
                                },
                            );
                        });
                    app.git_hook_pool.spawn_with_sink(
                        "task",
                        move || {
                            crate::git_routes::service::hook_commit(
                                &cfg,
                                &pkg,
                                "task",
                                &format!("task {} completed", task_id),
                                &sid_str,
                            );
                            Ok(())
                        },
                        Some(drop_notifier),
                    );
                }
            }
        } else if let Err(e) = result {
            // Demoted from `eprintln!`
            // and now carries the session-id so operators can correlate
            // failed updates with the tracing-pipeline `http` span for
            // the same request.
            tracing::warn!(
                ?session_id,
                error = %e,
                "progress session update failed"
            );
        }
    }

    if broadcast_state_advance {
        if let Some(s) = app.conversation.get_session(session_id).await {
            // discovery-approval blockers frequently reference
            // `runtime/outputs/<task_id>/decision.json` in the reason
            // without actually writing the file. Synthesize a stub so
            // the UI's BlockerCard radio picker resolves instead of
            // degrading to plain text.
            if let Some(pkg) = s.emitted_package_path.as_ref() {
                synthesize_missing_decision_json(pkg, &event.detail);
            }
            app.broadcast(
                session_id,
                SsePayload::StateAdvanced {
                    new_state: s.state.clone(),
                },
            )
            .await;
        }
    }

    // `task_stalled` transitions Emitted → Blocked
    // { Stalled {... } } atomically, then fires the matching SSE.
    if event.kind == "task_stalled" {
        let stall_signal = event.stall_signal.clone();
        let suggested_action = event
            .suggested_action
            .unwrap_or(ecaa_workflow_core::blocker::StallAction::Resize);
        let task_id = event.task_id.clone();
        if let Some(signal) = stall_signal.clone() {
            let store = app.conversation.store_handle();
            let signal_for_closure = signal.clone();
            let task_for_closure = task_id.clone();
            let _ = store
                .update(session_id, move |s| {
                    if matches!(
                        s.state,
                        ecaa_workflow_conversation::SessionState::Emitted
                            | ecaa_workflow_conversation::SessionState::Emitting
                    ) {
                        let blocker_kind = ecaa_workflow_core::blocker::BlockerKind::Stalled {
                            task_id: task_for_closure.clone(),
                            signal: signal_for_closure.clone(),
                            suggested_action,
                        };
                        let trigger = ecaa_workflow_conversation::session::StateTrigger::HarnessTaskBlocked {
                            task_id: task_for_closure,
                            detail: "stalled".into(),
                            blocker_kind,
                        };
                        if let Err(err) = s.try_transition(trigger.clone()) {
                            tracing::warn!(
                                session_id = %s.id,
                                trigger = ?trigger,
                                current_state = ?s.state,
                                error = ?err,
                                "illegal state transition ignored"
                            );
                        }
                    }
                    Ok(())
                })
                .await;
            app.broadcast(
                session_id,
                SsePayload::HarnessStallDetected {
                    task_id,
                    signal,
                    suggested_action,
                },
            )
            .await;
        }
    }

    // pilot lifecycle events forward the payload
    // straight to SSE. No session-state transition is required —
    // pilot outcomes surface in the Metrics tab and (on oversize)
    // through BlockerKind::PilotOversize, which the harness emits as
    // a separate `task_blocked` event already handled above.
    match event.kind.as_str() {
        "sizing_pilot_started" => {
            app.broadcast(session_id, SsePayload::HarnessSizingPilotStarted)
                .await;
        }
        "sizing_pilot_complete" => {
            let report = event.pilot_report.clone().unwrap_or(serde_json::json!({}));
            app.broadcast(
                session_id,
                SsePayload::HarnessSizingPilotComplete { report },
            )
            .await;
        }
        "sizing_pilot_skipped" => {
            app.broadcast(
                session_id,
                SsePayload::HarnessSizingPilotSkipped {
                    reason: event.detail.clone(),
                },
            )
            .await;
        }
        "cross_version_diff" => {
            let report = event
                .cross_version_report
                .clone()
                .unwrap_or(serde_json::json!({}));
            app.broadcast(session_id, SsePayload::HarnessVersionDiff { report })
                .await;
        }
        "resize_recommended" => {
            // The harness posts this alongside a
            // task_stalled event when the stall monitor projects that
            // a larger instance would resolve the stall. UI renders
            // the "resize recommended" chip in the Jobs tab via the
            // SsePayload::HarnessResizeRecommended variant.
            let from_instance_type = event.from_instance_type.clone().unwrap_or_default();
            let to_instance_type = event.to_instance_type.clone().unwrap_or_default();
            app.broadcast(
                session_id,
                SsePayload::HarnessResizeRecommended {
                    task_id: event.task_id.clone(),
                    from_instance_type,
                    to_instance_type,
                },
            )
            .await;
        }
        // harness startup diagnostic.
        "executor_selected" => {
            if let Some(info) = event.executor_info.as_ref() {
                app.broadcast(
                    session_id,
                    SsePayload::HarnessExecutorSelected {
                        name: info.name.clone(),
                        cpu_budget: info.cpu_budget,
                        gpu_budget: info.gpu_budget,
                        instance_type: info.instance_type.clone(),
                        harness_version: info.harness_version.clone(),
                        env_mode: info.env_mode.clone(),
                    },
                )
                .await;
            }
        }
        // POST-health report.
        "progress_client_health" => {
            if let Some(h) = event.client_health.as_ref() {
                app.broadcast(
                    session_id,
                    SsePayload::HarnessProgressHealth {
                        total_posts: h.total_posts,
                        failed_posts: h.failed_posts,
                        total_attempts: h.total_attempts,
                        last_error: h.last_error.clone(),
                        last_success_at: h.last_success_at.clone(),
                    },
                )
                .await;
            }
        }
        // verified AWS orphan-reap sweep.
        "orphan_instances_reaped" => {
            if let Some(r) = event.orphan_reap.as_ref() {
                app.broadcast(
                    session_id,
                    SsePayload::HarnessOrphansReaped {
                        candidate_count: r.candidate_count,
                        verified_count: r.verified_count,
                        unverified_ids: r.unverified_ids.clone(),
                        policy: r.policy.clone(),
                    },
                )
                .await;
            }
        }
        // detection
        // trigger (1). The agent writes a `disposition_proposed` progress
        // line pointing at the disposition file; we read + normalise
        // + enqueue + optionally auto-apply.
        "disposition_proposed" => {
            crate::chat_routes::dispositions::ingest_disposition_from_progress_event(
                &app,
                session_id,
                event.detail.as_str(),
            )
            .await;
        }
        // per-task heartbeat stall advisory. The
        // accompanying BlockerKind::HeartbeatStalled arrives as a
        // regular task_blocked event; this broadcast just lets the
        // Progress tab surface a chip without waiting for the state
        // transition. Non-terminal — handled before the state-machine
        // sync above (which skipped because event.kind starts with
        // "heartbeat_" and has no Running/Completed/Failed sibling).
        "heartbeat_stalled" => {
            if let Some(age) = event.heartbeat_age_secs {
                app.broadcast(
                    session_id,
                    SsePayload::HarnessHeartbeatStalled {
                        task_id: event.task_id.clone(),
                        age_secs: age,
                    },
                )
                .await;
            }
        }
        _ => {}
    }

    // Broadcast immediately so the UI's live progress feed stays responsive,
    // and enqueue into the batcher so the events also collapse into a single
    // assistant turn after the quiet window.
    let remote_session = event.remote.as_ref().map(|r| {
        ecaa_workflow_conversation::session::RemoteExecutionInfo {
            backend: r.backend.clone(),
            instance_id: r.instance_id.clone(),
            instance_type: r.instance_type.clone(),
        }
    });
    app.broadcast(
        session_id,
        SsePayload::HarnessProgress {
            kind: event.kind.clone(),
            task_id: event.task_id.clone(),
            status: event.status.clone(),
            detail: event.detail.clone(),
            remote: event.remote.clone(),
        },
    )
    .await;
    // second broadcast for task_completed events
    // that carry artifacts, so the UI can mark the card reviewable.
    if event.kind == "task_completed" {
        if let Some(artifacts) = event.artifacts.clone() {
            app.broadcast(
                session_id,
                SsePayload::TaskCompletedReviewable {
                    task_id: event.task_id.clone(),
                    artifacts,
                },
            )
            .await;
        }
        // trigger (2).
        // After every task completes, check whether the agent wrote
        // an adjacent `sme_disposition.json`. No-op when absent.
        if !event.task_id.is_empty() {
            crate::chat_routes::dispositions::scan_after_task_completed(
                &app,
                session_id,
                &event.task_id,
            )
            .await;
        }
        // auto-fire the narrative
        // dashboard summary when the reporting stage completes. The
        // side-call module caches by source fingerprint, so re-entry
        // (re-run, amendment) will re-summarise only if the source
        // materially changed. Detached task: the user-facing progress
        // path must not wait on a Haiku round-trip.
        if event.task_id == "final_reporting" {
            let app_clone = app.clone();
            tokio::spawn(async move {
                if let Some(sess) = app_clone.conversation.get_session(session_id).await {
                    let source = crate::chat_routes::summary::build_source(&sess);
                    if source.trim().is_empty() {
                        return;
                    }
                    use std::collections::hash_map::DefaultHasher;
                    use std::hash::{Hash, Hasher};
                    let mut hasher = DefaultHasher::new();
                    source.hash(&mut hasher);
                    let fingerprint = format!("{:016x}", hasher.finish());
                    let backend = app_clone.conversation.llm_for_scoring();
                    let metrics = app_clone.conversation.metrics();
                    // Plan S2.5 — retry once on transient failure
                    // before surfacing to the UI. Most transient
                    // failures are 429 rate-limits at the side-call
                    // billing seam; a single backoff-and-retry covers
                    // ~95% per Anthropic-side observability.
                    let mut last_err = None;
                    for attempt in 0..2u32 {
                        match ecaa_workflow_conversation::side_calls::summary::generate_dashboard_summary(
                            backend.clone(), &metrics.clone(), session_id, &source, &fingerprint,
                        )
                        .await
                        {
                            Ok(_) => {
                                last_err = None;
                                break;
                            }
                            Err(err) => {
                                last_err = Some(err);
                                if attempt == 0 {
                                    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                                }
                            }
                        }
                    }
                    if let Some(err) = last_err {
                        // Demoted
                        // from `eprintln!` so the session-id surface
                        // stays in the structured tracing pipeline.
                        tracing::debug!(
                            ?session_id,
                            error = %err,
                            "dashboard-summary-auto failed after retry"
                        );
                        // Fan out the failure to the UI so the SME
                        // can see why the Dashboard summary tab is
                        // empty and retry from the banner. Use
                        // `app.broadcast` so the event picks up a
                        // monotonic seq via the envelope.
                        app_clone
                            .broadcast(
                                session_id,
                                SsePayload::DashboardSummaryFailed {
                                    task_id: "final_reporting".into(),
                                    reason: err.to_string(),
                                },
                            )
                            .await;
                    }
                }
            });
        }
    }
    let harness_event = HarnessEvent {
        kind: event.kind,
        task_id: event.task_id,
        status: event.status,
        detail: event.detail,
        remote: remote_session,
        timestamp: chrono::Utc::now(),
    };
    app.batcher.clone().enqueue(session_id, harness_event).await;

    // When the harness included `client_now` (first POST handshake),
    // echo the server's current time so the harness can measure
    // host-vs-server clock skew (§9.1). The header is omitted on
    // all subsequent POSTs where `client_now` is absent — older
    // harness builds that don't send the field are unaffected.
    if event.client_now.is_some() {
        let server_now = chrono::Utc::now().to_rfc3339();
        let mut headers = HeaderMap::new();
        if let Ok(val) = HeaderValue::from_str(&server_now) {
            headers.insert("X-Server-Now", val);
        }
        return (StatusCode::NO_CONTENT, headers).into_response();
    }

    StatusCode::NO_CONTENT.into_response()
}
