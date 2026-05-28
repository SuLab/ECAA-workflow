//! Turn + checkpoint handlers. `send_turn` runs one LLM tool-use loop
//! against the session. `confirm` / `reject` / `unblock` are the
//! user-driven state transitions that follow a
//! `propose_summary_confirmation` card or a `Blocked` state.

use super::*;
use axum::{extract::State, http::StatusCode, response::IntoResponse, Json};
use uuid::Uuid;

/// `POST /api/chat/session/:id/turn` — append a user message and run the LLM tool loop.
#[tracing::instrument(skip(app, req), fields(session_id = %session_id))]
pub async fn send_turn(
    State(app): State<ChatAppState>,
    Path(session_id): Path<Uuid>,
    BoundedJson(req): BoundedJson<SendTurnRequest>,
) -> impl IntoResponse {
    // Per-session
    // per-minute cap on `/turn`. 30 / minute lets a normal SME hold a
    // brisk back-and-forth while a runaway client can't bill the
    // session into oblivion. Read-only endpoints (state / transcript /
    // metrics) are protected by the global per-IP governor.
    if let Err(status) =
        LlmRateBuckets::check(&app.llm_buckets.turn, session_id, app.llm_rate_limits.turn)
    {
        return (
            status,
            "rate limit exceeded: /turn capped at 30/min/session",
        )
            .into_response();
    }

    // Capture pre-turn state so the post-turn diff can detect the
    // ReadyToEmit/Emitting → Emitted transition that fires the git
    // commit hook. The legacy /confirm path only advances PendingConfirmation
    // → ReadyToEmit; Emitted happens later inside the tool loop driven
    // by send_turn — so the emit-hook lives here, not in /confirm.
    let pre_state = app
        .conversation
        .get_session(session_id)
        .await
        .map(|s| s.state.clone());

    match app
        .conversation
        .send_turn(session_id, req.message, req.user_turn_id)
        .await
    {
        Ok(turn) => {
            // when the turn finished with the session in
            // Emitted and a cross-version diff is present on disk, fan
            // out `HarnessVersionDiff` so the UI timeline + result card
            // update without polling. No-op when no diff exists.
            if let Some(s) = app.conversation.get_session(session_id).await {
                let now_emitted = matches!(
                    s.state,
                    ecaa_workflow_conversation::SessionState::Emitted
                );
                if now_emitted {
                    if let Some(pkg) = &s.emitted_package_path {
                        let diff_path = pkg.join("runtime").join("cross-version-diff.json");
                        if let Ok(bytes) = tokio::fs::read(&diff_path).await {
                            if let Ok(report) = serde_json::from_slice::<serde_json::Value>(&bytes)
                            {
                                app.broadcast(
                                    session_id,
                                    SsePayload::HarnessVersionDiff { report },
                                )
                                .await;
                            }
                        }
                    }
                }
                // Git emit-hook: fires on the ReadyToEmit/Emitting →
                // Emitted transition observed across this turn. Pre-state
                // must be ReadyToEmit or Emitting AND post-state must be
                // Emitted AND the session must have an emitted package
                // path. Anything else is either a re-emission (Emitted →
                // Emitted, no commit) or a non-emission turn.
                let was_pre_emission = matches!(
                    pre_state,
                    Some(ecaa_workflow_conversation::SessionState::ReadyToEmit)
                        | Some(ecaa_workflow_conversation::SessionState::Emitting)
                );
                if was_pre_emission && now_emitted {
                    if let Some(pkg) = s.emitted_package_path.clone() {
                        let cfg = app.git_config().read().clone();
                        let sid_str = session_id.to_string();
                        let subject = s.title.clone().unwrap_or_else(|| {
                            format!("session {}", &sid_str[..8.min(sid_str.len())])
                        });
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
                            "emit",
                            move || {
                                crate::git_routes::service::hook_commit(
                                    &cfg, &pkg, "emit", &subject, &sid_str,
                                );
                                Ok(())
                            },
                            Some(drop_notifier),
                        );
                    }
                }
            }
            Json(turn).into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

/// `POST /api/chat/session/:id/confirm` — SME confirmation; advances `PendingConfirmation → ReadyToEmit`.
#[tracing::instrument(skip(app, headers, body), fields(session_id = %session_id))]
pub async fn confirm(
    State(app): State<ChatAppState>,
    Path(session_id): Path<Uuid>,
    headers: axum::http::HeaderMap,
    body: Option<BoundedJson<CheckpointDecisionRequest>>,
) -> axum::response::Response {
    // R3.8: If-Match precondition. Optimistic-concurrency check —
    // a stale client retrying a stale view must not silently win the
    // race against a fresh state mutation. Run before the idempotency
    // short-circuit so a 412 doesn't get cached as a "successful" reply.
    if let Some(s) = app.conversation.get_session(session_id).await {
        if let super::IfMatchOutcome::Mismatch { server, client } =
            super::check_if_match(&headers, &s, "confirm")
        {
            return super::precondition_failed_response(&server, &client);
        }
    }
    // `Idempotency-Key` short-circuit. A retry
    // within `ECAA_IDEMPOTENCY_TTL_SECS` (default 1h) with the same
    // header value replays the cached response.
    let ticket = app.idempotency.lookup(session_id, "confirm", &headers);
    if let Some(replay) = ticket.cached_response() {
        return replay;
    }
    let response = confirm_inner(app.clone(), session_id, body).await;
    ticket.store(&app.idempotency, response).await
}

async fn confirm_inner(
    app: ChatAppState,
    session_id: Uuid,
    body: Option<BoundedJson<CheckpointDecisionRequest>>,
) -> axum::response::Response {
    let (rationale, stage, mode, checkpoint_mode) = body
        .map(|BoundedJson(b)| (b.rationale, b.stage, b.mode, b.checkpoint_mode))
        .unwrap_or((None, None, None, None));
    // Stage-scoped confirm: write a sidecar recording the
    // stage unblock so the harness scheduler can skip the SME gate
    // on subsequent iterations. Emission-level confirms (stage=None)
    // flow through the existing `confirm_with_rationale` path
    // unchanged, advancing PendingConfirmation → ReadyToEmit.
    if let Some(stage_id) = stage.as_deref() {
        // Path-jail: jail the body-supplied `stage_id` before
        // formatting it into the sidecar filename. Without the jail a
        // request like `{"stage": "../../tmp/escape"}` would collapse
        // to a write outside the package root; safe_segment_join
        // rejects '..' / absolute / separator-bearing stage_ids with 400.
        let filename = format!("sme-review-confirmed-{}.json", stage_id);
        if let Some(s) = app.conversation.get_session(session_id).await {
            if let Some(pkg) = &s.emitted_package_path {
                let runtime_dir = pkg.join("runtime");
                let path = match super::safe_segment_join(&runtime_dir, &filename) {
                    Ok(p) => p,
                    Err(e) => {
                        return (StatusCode::BAD_REQUEST, format!("invalid stage_id: {}", e))
                            .into_response();
                    }
                };
                let body = serde_json::json!({
                    "stage": stage_id,
                    "confirmed_at": ecaa_workflow_core::time_helpers::now_rfc3339(),
                    "rationale": rationale.clone(),
                });
                // Best-effort; a write failure surfaces
                // to the caller but leaves the session in its pre-
                // confirm state. The previous `path.parent().unwrap()`
                // would panic if `path` somehow had no parent — load-
                // bearing only on a malformed `pkg` upstream, but the
                // request-path panic surface was a real risk; the
                // rewrite returns 500 with the missing-parent reason
                // instead.
                let Some(parent) = path.parent() else {
                    return (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        format!(
                            "sme-review-confirmed sidecar path has no parent: {}",
                            path.display()
                        ),
                    )
                        .into_response();
                };
                if let Err(e) = std::fs::create_dir_all(parent) {
                    return (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        format!("creating runtime dir: {}", e),
                    )
                        .into_response();
                }
                // Belt-and-suspenders canonicalize check now that the
                // parent exists. Rejects symlink-based escapes the
                // segment-only check would miss.
                if let Err(e) = super::assert_under_root(pkg, &path) {
                    return (StatusCode::FORBIDDEN, format!("path escapes root: {}", e))
                        .into_response();
                }
                if let Err(e) = std::fs::write(
                    &path,
                    serde_json::to_string_pretty(&body).unwrap_or_default(),
                ) {
                    return (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        format!("writing sme-review-confirmed sidecar: {}", e),
                    )
                        .into_response();
                }
            }
        }
        // Don't advance the overall session state — per-stage
        // confirms only unblock the scheduler gate.
        return StatusCode::NO_CONTENT.into_response();
    }
    match app
        .conversation
        .confirm_with_modes(session_id, rationale, mode, checkpoint_mode)
        .await
    {
        Ok(()) => {
            // RCA F9: deterministic auto-fire of emit_package. The
            // `/confirm` flip of `user_confirmed = true` opens the only
            // gate `emit_package` reads; waiting for the LLM to call
            // the tool on its next turn lets the model litigate scope
            // (and burn input tokens) over a gate the server already
            // opened. Run the dispatcher directly under the same
            // per-session lock so the SME-click → emitted-package
            // latency drops to milliseconds.
            //
            // try_auto_emit_after_confirm internally skips when the
            // session has pending atom proposals awaiting signoff, the
            // confirmation token is stale, or the state machine isn't
            // sitting at ReadyToEmit. Any of those paths leaves the
            // legacy LLM-driven emit_package call intact on the next
            // turn — auto-emit is a front-runner, not a replacement.
            let auto_emit = match app
                .conversation
                .try_auto_emit_after_confirm(session_id)
                .await
            {
                Ok(outcome) => outcome,
                Err(e) => {
                    tracing::warn!(
                        session_id = %session_id,
                        error = %e,
                        "auto_emit_after_confirm failed; leaving session in \
                         ReadyToEmit for the LLM path to handle"
                    );
                    None
                }
            };
            if let Some(s) = app.conversation.get_session(session_id).await {
                app.broadcast(
                    session_id,
                    SsePayload::StateAdvanced {
                        new_state: s.state.clone(),
                    },
                )
                .await;
                // Auto-emit fan-out: when the dispatcher just
                // advanced ReadyToEmit → Emitted in the lock-holding
                // transaction above, mirror the post-emit logic that
                // `send_turn` runs for an LLM-driven emit_package
                // (cross-version diff broadcast + git emit hook).
                // Pure code reuse of the same SsePayload contract +
                // git_hook_pool surface so SSE consumers and the
                // provenance git tree can't tell auto-emit from the
                // legacy path.
                if auto_emit.is_some()
                    && matches!(
                        s.state,
                        ecaa_workflow_conversation::SessionState::Emitted
                    )
                {
                    if let Some(pkg) = &s.emitted_package_path {
                        let diff_path = pkg.join("runtime").join("cross-version-diff.json");
                        if let Ok(bytes) = tokio::fs::read(&diff_path).await {
                            if let Ok(report) = serde_json::from_slice::<serde_json::Value>(&bytes)
                            {
                                app.broadcast(
                                    session_id,
                                    SsePayload::HarnessVersionDiff { report },
                                )
                                .await;
                            }
                        }
                    }
                    if let Some(pkg) = s.emitted_package_path.clone() {
                        let cfg = app.git_config().read().clone();
                        let sid_str = session_id.to_string();
                        let subject = s.title.clone().unwrap_or_else(|| {
                            format!("session {}", &sid_str[..8.min(sid_str.len())])
                        });
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
                            "emit",
                            move || {
                                crate::git_routes::service::hook_commit(
                                    &cfg, &pkg, "emit", &subject, &sid_str,
                                );
                                Ok(())
                            },
                            Some(drop_notifier),
                        );
                    }
                }
            }
            StatusCode::NO_CONTENT.into_response()
        }
        Err(e) => {
            // / D8 + §8.F.4: the service surfaces
            // "precondition_failure:..." for the two confirm-time
            // rejections (ClinicalTrial missing mode, confirmatory +
            // Fast). emit the typed `ApiError`
            // envelope so the UI can branch on `code` rather than
            // substring-matching the body.
            let msg = e.to_string();
            if msg.contains("precondition_failure") {
                crate::error::ApiError::PreconditionFailed(msg).into_response()
            } else {
                crate::error::ApiError::BadRequest(msg).into_response()
            }
        }
    }
}

/// `POST /api/chat/session/:id/reject` — SME rejection; reverts `PendingConfirmation → Intake`.
#[tracing::instrument(skip(app, headers, body), fields(session_id = %session_id))]
pub async fn reject(
    State(app): State<ChatAppState>,
    Path(session_id): Path<Uuid>,
    headers: axum::http::HeaderMap,
    body: Option<BoundedJson<CheckpointDecisionRequest>>,
) -> axum::response::Response {
    // R3.8: If-Match precondition (see `confirm` for rationale).
    if let Some(s) = app.conversation.get_session(session_id).await {
        if let super::IfMatchOutcome::Mismatch { server, client } =
            super::check_if_match(&headers, &s, "reject")
        {
            return super::precondition_failed_response(&server, &client);
        }
    }
    let rationale = body.and_then(|BoundedJson(b)| b.rationale);
    match app
        .conversation
        .reject_with_rationale(session_id, rationale)
        .await
    {
        Ok(()) => {
            if let Some(s) = app.conversation.get_session(session_id).await {
                app.broadcast(
                    session_id,
                    SsePayload::StateAdvanced {
                        new_state: s.state.clone(),
                    },
                )
                .await;
            }
            StatusCode::NO_CONTENT.into_response()
        }
        Err(e) => (StatusCode::BAD_REQUEST, e.to_string()).into_response(),
    }
}

/// `POST /api/chat/session/:id/unblock` — resolve a blocker and optionally auto-relaunch the harness.
#[tracing::instrument(skip(app, headers, body), fields(session_id = %session_id))]
pub async fn unblock(
    State(app): State<ChatAppState>,
    Path(session_id): Path<Uuid>,
    headers: axum::http::HeaderMap,
    body: Option<BoundedJson<CheckpointDecisionRequest>>,
) -> axum::response::Response {
    // R3.8: If-Match precondition (see `confirm` for rationale).
    if let Some(s) = app.conversation.get_session(session_id).await {
        if let super::IfMatchOutcome::Mismatch { server, client } =
            super::check_if_match(&headers, &s, "unblock")
        {
            return super::precondition_failed_response(&server, &client);
        }
    }
    let (rationale, resolution) = body
        .map(|BoundedJson(b)| (b.rationale, b.resolution))
        .unwrap_or((None, None));
    match app
        .conversation
        .unblock_with_rationale(session_id, rationale)
        .await
    {
        Ok(()) => {
            let session = app.conversation.get_session(session_id).await;

            // stall recovery routing. When the SME
            // picked `resize` or `abort` from the BlockerCard buttons,
            // apply the resolution before the generic resume path runs.
            if let (Some(res), Some(s)) = (resolution.as_deref(), &session) {
                if let Some(pkg) = &s.emitted_package_path {
                    match res {
                        "resize" => {
                            // The harness reads this sidecar on its
                            // next iteration and bumps the instance
                            // shape before re-running the stalled task.
                            let path = pkg.join("runtime").join("resize-request.json");
                            let body = serde_json::json!({
                                "requested_at": ecaa_workflow_core::time_helpers::now_rfc3339(),
                            });
                            if let Some(parent) = path.parent() {
                                let _ = tokio::fs::create_dir_all(parent).await;
                            }
                            if let Err(e) = tokio::fs::write(&path, body.to_string()).await {
                                eprintln!("[unblock] resize-request write failed: {}", e);
                            }
                        }
                        "abort" => {
                            if let Err(e) = execution::fail_blocked_tasks_in_workflow(pkg).await {
                                eprintln!("[unblock] abort (fail tasks) failed: {}", e);
                            }
                        }
                        _ => {}
                    }
                }
            }

            // if the session has an emitted package AND any
            // tasks in WORKFLOW.json are in Blocked state, flip them back
            // to Ready so the harness resumes execution. This closes the
            // full-lifecycle loop: SME clicks Unblock → session state
            // advances AND the DAG task resumes. Silent no-op for
            // intake-phase blockers (no package path yet).
            //
            // Skipped when resolution == "abort" since those tasks were
            // just marked Failed above.
            if resolution.as_deref() != Some("abort") {
                if let Some(s) = &session {
                    // Resume both the session's emitted_package_path
                    // AND the live execution's package_dir — they
                    // diverge when emit_package fires twice (observed
                    // in the IVD live spec: the agent's running
                    // package is the FIRST emission, but state was
                    // updated to the SECOND). Resuming only the state
                    // path leaves the harness's actual WORKFLOW.json
                    // stuck at Blocked forever.
                    let mut paths: Vec<std::path::PathBuf> = Vec::new();
                    if let Some(pkg) = &s.emitted_package_path {
                        paths.push(pkg.clone());
                    }
                    if let Some(exec) = app.executions.get(&session_id) {
                        let pkg_dir = exec.value().package_dir.clone();
                        if !paths.iter().any(|p| p == &pkg_dir) {
                            paths.push(pkg_dir);
                        }
                    }
                    for pkg in &paths {
                        if let Err(e) = execution::resume_blocked_tasks_in_workflow(pkg).await {
                            eprintln!(
                                "[unblock] WORKFLOW.json resume failed for {}: {}",
                                pkg.display(),
                                e
                            );
                        }
                    }
                }
            }

            if let Some(s) = session {
                app.broadcast(
                    session_id,
                    SsePayload::StateAdvanced {
                        new_state: s.state.clone(),
                    },
                )
                .await;
            }

            // Auto-relaunch hook. After unblocking a Blocked session the
            // overwhelming intent is "resume execution now"; rather than
            // make the SME click a separate Resume button, spawn a new
            // harness iff the gate predicate is satisfied. Debounced
            // against an unblock/re-block/unblock ping-pong leaking
            // processes. Logged to stderr on skip/error; never 500s.
            execution::maybe_auto_relaunch_harness(&app, session_id, "unblock").await;

            StatusCode::NO_CONTENT.into_response()
        }
        Err(e) => (StatusCode::BAD_REQUEST, e.to_string()).into_response(),
    }
}

/// Server-side SME-method-signal endpoint. The UI calls this when the
/// SME clicks a method quick-reply chip or types a method name
/// unprompted in a structured intake form. The flag-flip is recorded
/// on `Session.sme_method_signals.named` and gates the subsequent LLM
/// `set_intake_method` call (refused with `precondition_failure`
/// until this endpoint runs for the stage).
///
/// Returns `204 NO_CONTENT` on success. The `stage_id` path parameter
/// is taken verbatim — `set_intake_method` already normalizes the
/// `discover_` prefix on its end so callers can use either form.
#[tracing::instrument(skip(app), fields(session_id = %session_id, stage_id = %stage_id))]
pub(super) async fn post_sme_named_method(
    State(app): State<ChatAppState>,
    Path((session_id, stage_id)): Path<(Uuid, String)>,
) -> impl IntoResponse {
    if stage_id.trim().is_empty() {
        return (StatusCode::BAD_REQUEST, "stage_id must be non-empty").into_response();
    }
    // Path-jail belt-and-suspenders: even though `stage_id` only
    // becomes a BTreeMap key (not a filesystem path), reject the same
    // separator / traversal patterns the rest of the chat-routes
    // surface enforces so the flag map's keyspace stays well-formed.
    if stage_id.contains('/')
        || stage_id.contains('\\')
        || stage_id.contains("..")
        || stage_id.contains('\0')
    {
        return (
            StatusCode::BAD_REQUEST,
            "stage_id contains invalid characters",
        )
            .into_response();
    }
    let normalized = stage_id
        .strip_prefix("discover_")
        .unwrap_or(&stage_id)
        .to_string();
    let store = app.conversation.store_handle();
    let result = store
        .update(session_id, move |s| {
            s.sme_method_signals.named.insert(normalized.clone(), true);
            Ok(())
        })
        .await;
    match result {
        Ok(_) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("failed to record SME-named method: {}", e),
        )
            .into_response(),
    }
}

/// Route inventory for the doc-as-contract gate +
/// per-submodule `routes()` builder. `mod.rs::router()` merges every
/// submodule's builder into the single chat surface.
pub(super) const ROUTES: &[(&str, &str)] = &[
    ("POST", "/api/chat/session/:id/turn"),
    ("POST", "/api/chat/session/:id/confirm"),
    ("POST", "/api/chat/session/:id/reject"),
    ("POST", "/api/chat/session/:id/unblock"),
    (
        "POST",
        "/api/chat/session/:id/intake-method/:stage_id/sme-named",
    ),
];

pub(super) fn routes() -> axum::Router<ChatAppState> {
    axum::Router::new()
        .route("/api/chat/session/:id/turn", axum::routing::post(send_turn))
        .route(
            "/api/chat/session/:id/confirm",
            axum::routing::post(confirm),
        )
        .route("/api/chat/session/:id/reject", axum::routing::post(reject))
        .route(
            "/api/chat/session/:id/unblock",
            axum::routing::post(unblock),
        )
        .route(
            "/api/chat/session/:id/intake-method/:stage_id/sme-named",
            axum::routing::post(post_sme_named_method),
        )
}

#[cfg(test)]
mod tests {
    use crate::chat_routes::test_support::{
        assistant, body_json, make_router, seed_session_with_completed_task, tool_use,
    };
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use ecaa_workflow_conversation::{BatchableTool, Tool};
    use tower::util::ServiceExt;

    struct EnvVarGuard {
        key: &'static str,
        previous: Option<String>,
    }

    impl EnvVarGuard {
        fn set(key: &'static str, value: &str) -> Self {
            let previous = std::env::var(key).ok();
            std::env::set_var(key, value);
            Self { key, previous }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            match &self.previous {
                Some(value) => std::env::set_var(self.key, value),
                None => std::env::remove_var(self.key),
            }
        }
    }

    #[tokio::test]
    async fn send_turn_drives_tool_loop() {
        let (router, _) = make_router(vec![
            tool_use(Tool::Batchable(BatchableTool::AppendIntakeProse {
                prose: "single cell scRNA-seq human IVD samples".into(),
            })),
            assistant("Got it — single-cell."),
        ])
        .await;

        let req = Request::builder()
            .method("POST")
            .uri("/api/chat/session")
            .header("content-type", "application/json")
            .body(Body::from(r#"{}"#))
            .unwrap();
        let resp = router.clone().oneshot(req).await.unwrap();
        let body = body_json(resp.into_body()).await;
        let session_id = body["session_id"].as_str().unwrap().to_string();

        let req = Request::builder()
            .method("POST")
            .uri(format!("/api/chat/session/{}/turn", session_id))
            .header("content-type", "application/json")
            .body(Body::from(r#"{"message":"hello"}"#))
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp.into_body()).await;
        assert!(body["content"].as_str().unwrap().contains("single-cell"));
    }

    #[tokio::test]
    async fn confirm_advances_state_via_endpoint() {
        let (router, _) = make_router(vec![
            tool_use(Tool::Batchable(BatchableTool::AppendIntakeProse {
                prose: "single cell scRNA-seq human samples".into(),
            })),
            tool_use(Tool::Batchable(BatchableTool::ProposeSummaryConfirmation {
                summary_markdown: "Plan ready".into(),
            })),
            assistant("Confirm?"),
        ])
        .await;

        let req = Request::builder()
            .method("POST")
            .uri("/api/chat/session")
            .header("content-type", "application/json")
            .body(Body::from(r#"{}"#))
            .unwrap();
        let resp = router.clone().oneshot(req).await.unwrap();
        let session_id = body_json(resp.into_body()).await["session_id"]
            .as_str()
            .unwrap()
            .to_string();

        let req = Request::builder()
            .method("POST")
            .uri(format!("/api/chat/session/{}/turn", session_id))
            .header("content-type", "application/json")
            .body(Body::from(r#"{"message":"go"}"#))
            .unwrap();
        let _ = router.clone().oneshot(req).await.unwrap();

        let req = Request::builder()
            .method("POST")
            .uri(format!("/api/chat/session/{}/confirm", session_id))
            .body(Body::empty())
            .unwrap();
        let resp = router.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);

        let req = Request::builder()
            .method("GET")
            .uri(format!("/api/chat/session/{}/state", session_id))
            .body(Body::empty())
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        let body = body_json(resp.into_body()).await;
        // /confirm now auto-emits when the session is in ReadyToEmit so
        // the state lands at `emitted`. Post-D9: an Emitted session with
        // a durable `emitted_package_path` reports `user_confirmed=true`
        // because the on-disk RO-Crate IS the artifact of the prior
        // SME confirmation, even though the per-emit `ConfirmationToken`
        // was consumed by `emit_package_post_ok`. Without this short-
        // circuit a server restart would make the LLM see
        // `user_confirmed=false` on a session that has already emitted
        // and prompt the SME to re-click Confirm.
        assert_eq!(body["state"]["kind"], "emitted");
        assert_eq!(body["user_confirmed"], true);
    }

    /// Catches the deadlock where `try_auto_emit_after_confirm` held the
    /// per-session `tokio::sync::Mutex` via `store.transaction(...)` and
    /// `emit_package` re-entered the same lock via `store.get(...)` for
    /// its fresh-read of `user_confirmed` / proposals. The /confirm POST
    /// stalled until the upstream 300s axum timer fired with 408, the
    /// session was stranded in `emitting`, and zero packages ever reached
    /// disk. Wrap the request in a 10s timeout so a future re-entrant
    /// lock fails fast instead of stalling CI.
    #[tokio::test]
    async fn confirm_auto_emit_does_not_deadlock() {
        let _validation_guard = EnvVarGuard::set("ECAA_VALIDATE_ON_EMIT", "schema_only");
        let (router, _) = make_router(vec![
            tool_use(Tool::Batchable(BatchableTool::AppendIntakeProse {
                prose: "single cell scRNA-seq human samples".into(),
            })),
            tool_use(Tool::Batchable(BatchableTool::ProposeSummaryConfirmation {
                summary_markdown: "Plan ready".into(),
            })),
            assistant("Confirm?"),
        ])
        .await;

        let req = Request::builder()
            .method("POST")
            .uri("/api/chat/session")
            .header("content-type", "application/json")
            .body(Body::from(r#"{}"#))
            .unwrap();
        let resp = router.clone().oneshot(req).await.unwrap();
        let session_id = body_json(resp.into_body()).await["session_id"]
            .as_str()
            .unwrap()
            .to_string();

        let req = Request::builder()
            .method("POST")
            .uri(format!("/api/chat/session/{}/turn", session_id))
            .header("content-type", "application/json")
            .body(Body::from(r#"{"message":"go"}"#))
            .unwrap();
        let _ = router.clone().oneshot(req).await.unwrap();

        let req = Request::builder()
            .method("POST")
            .uri(format!("/api/chat/session/{}/confirm", session_id))
            .body(Body::empty())
            .unwrap();
        let resp = tokio::time::timeout(std::time::Duration::from_secs(10), router.oneshot(req))
            .await
            .expect("confirm endpoint hung — auto-emit deadlock regression")
            .expect("router did not complete the confirm request");
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    }

    // ── ClinicalTrial force-explicit-mode at /confirm ──────

    #[tokio::test]
    async fn clinical_trial_confirm_without_mode_returns_precondition_failed() {
        // Regression guard. The test pins `session.project_class`
        // via the store's update handle to sidestep classifier drift
        // — the gate under test is "confirm rejects without explicit
        // mode when class==ClinicalTrial," not "classifier routes SAP
        // prose to ClinicalTrial" (covered directly in the conversation
        // crate's `clinical_trial_prose_loads_clinical_taxonomy` test).
        let (router, app) = make_router(vec![
            tool_use(Tool::Batchable(BatchableTool::AppendIntakeProse {
                prose: "Bulk rna-seq differential expression.".into(),
            })),
            tool_use(Tool::Batchable(BatchableTool::ProposeSummaryConfirmation {
                summary_markdown: "Plan ready".into(),
            })),
            assistant("Confirm?"),
        ])
        .await;

        let req = Request::builder()
            .method("POST")
            .uri("/api/chat/session")
            .header("content-type", "application/json")
            .body(Body::from(r#"{}"#))
            .unwrap();
        let resp = router.clone().oneshot(req).await.unwrap();
        let session_id = body_json(resp.into_body()).await["session_id"]
            .as_str()
            .unwrap()
            .to_string();
        let sid = session_id.parse::<uuid::Uuid>().unwrap();

        let req = Request::builder()
            .method("POST")
            .uri(format!("/api/chat/session/{}/turn", session_id))
            .header("content-type", "application/json")
            .body(Body::from(r#"{"message":"go"}"#))
            .unwrap();
        let _ = router.clone().oneshot(req).await.unwrap();

        // Pin the class on the session directly so the precondition
        // path fires regardless of the initial prose classification.
        app.conversation
            .store_handle()
            .update(sid, |s| {
                s.project_class = ecaa_workflow_core::project_class::ProjectClass::ClinicalTrial;
                Ok(())
            })
            .await
            .unwrap();

        // First confirm without `mode` → 412.
        let req = Request::builder()
            .method("POST")
            .uri(format!("/api/chat/session/{}/confirm", session_id))
            .header("content-type", "application/json")
            .body(Body::from(r#"{}"#))
            .unwrap();
        let resp = router.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::PRECONDITION_FAILED);

        // With explicit mode=exploratory → 204.
        let req = Request::builder()
            .method("POST")
            .uri(format!("/api/chat/session/{}/confirm", session_id))
            .header("content-type", "application/json")
            .body(Body::from(r#"{"mode":{"kind":"exploratory"}}"#))
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn unblock_flips_blocked_tasks_in_workflow_back_to_ready() {
        let pkg = tempfile::tempdir().unwrap();
        let wf_path = pkg.path().join("WORKFLOW.json");
        std::fs::write(
            &wf_path,
            r#"{
                    "version": "1.0",
                    "workflow_id": "w-1",
                    "current_task": null,
                    "tasks": {
                        "discover_quant": {
                            "kind": {"discovery":"best_practice"},
                            "state": {"status":"blocked","record":{"reason":"mock","attempts":[]}},
                            "depends_on": [],
                            "assignee": "agent",
                            "description": "d"
                        },
                        "alignment": {
                            "kind": "computation",
                            "state": {"status":"ready"},
                            "depends_on": [],
                            "assignee": "agent",
                            "description": "a"
                        }
                    }
                }"#,
        )
        .unwrap();

        let (router, app) = make_router(vec![]).await;
        let id =
            seed_session_with_completed_task(&app, "t_demo", Some(pkg.path().to_path_buf())).await;

        let store = app.conversation.store_handle();
        store
            .update(id, |s| {
                s.state = ecaa_workflow_conversation::SessionState::Blocked {
                    blockers: vec![],
                    reason: "mock".into(),
                    recovery_hint: "unblock to resume".into(),
                    blocker_kind: None,
                    context: None,
                };
                Ok(())
            })
            .await
            .unwrap();

        let req = Request::builder()
            .method("POST")
            .uri(format!("/api/chat/session/{}/unblock", id))
            .body(Body::empty())
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);

        let raw = std::fs::read_to_string(&wf_path).unwrap();
        let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(
            v["tasks"]["discover_quant"]["state"]["status"].as_str(),
            Some("ready"),
        );
        assert_eq!(
            v["tasks"]["alignment"]["state"]["status"].as_str(),
            Some("ready"),
        );
        drop(pkg);
    }

    #[tokio::test]
    async fn unblock_with_resize_resolution_writes_sidecar() {
        let pkg = tempfile::tempdir().unwrap();
        let wf_path = pkg.path().join("WORKFLOW.json");
        std::fs::write(
            &wf_path,
            r#"{
                    "version": "1.0",
                    "workflow_id": "w-1",
                    "current_task": null,
                    "tasks": {
                        "align_stalled": {
                            "kind": "computation",
                            "state": {"status":"blocked","record":{"reason":"stalled","attempts":[]}},
                            "depends_on": [],
                            "assignee": "agent",
                            "description": "stalled task"
                        }
                    }
                }"#,
        )
        .unwrap();

        let (router, app) = make_router(vec![]).await;
        let id =
            seed_session_with_completed_task(&app, "t_demo", Some(pkg.path().to_path_buf())).await;

        let store = app.conversation.store_handle();
        store
            .update(id, |s| {
                s.state = ecaa_workflow_conversation::SessionState::Blocked {
                    blockers: vec![],
                    reason: "stalled".into(),
                    recovery_hint: "resize to resume".into(),
                    blocker_kind: None,
                    context: None,
                };
                Ok(())
            })
            .await
            .unwrap();

        let req = Request::builder()
            .method("POST")
            .uri(format!("/api/chat/session/{}/unblock", id))
            .header("content-type", "application/json")
            .body(Body::from(r#"{"resolution":"resize"}"#))
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);

        let sidecar = pkg.path().join("runtime").join("resize-request.json");
        assert!(
            sidecar.exists(),
            "resize-request.json must exist at {}",
            sidecar.display()
        );
        let raw = std::fs::read_to_string(&sidecar).unwrap();
        let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert!(v["requested_at"].is_string());
        drop(pkg);
    }

    #[tokio::test]
    async fn unblock_with_abort_resolution_marks_tasks_failed() {
        let pkg = tempfile::tempdir().unwrap();
        let wf_path = pkg.path().join("WORKFLOW.json");
        std::fs::write(
            &wf_path,
            r#"{
                    "version": "1.0",
                    "workflow_id": "w-1",
                    "current_task": null,
                    "tasks": {
                        "align_stalled": {
                            "kind": "computation",
                            "state": {"status":"blocked","record":{"reason":"stalled","attempts":[]}},
                            "depends_on": [],
                            "assignee": "agent",
                            "description": "stalled task"
                        },
                        "not_blocked": {
                            "kind": "computation",
                            "state": {"status":"ready"},
                            "depends_on": [],
                            "assignee": "agent",
                            "description": "other"
                        }
                    }
                }"#,
        )
        .unwrap();

        let (router, app) = make_router(vec![]).await;
        let id =
            seed_session_with_completed_task(&app, "t_demo", Some(pkg.path().to_path_buf())).await;

        let store = app.conversation.store_handle();
        store
            .update(id, |s| {
                s.state = ecaa_workflow_conversation::SessionState::Blocked {
                    blockers: vec![],
                    reason: "stalled".into(),
                    recovery_hint: "abort to stop retrying".into(),
                    blocker_kind: None,
                    context: None,
                };
                Ok(())
            })
            .await
            .unwrap();

        let req = Request::builder()
            .method("POST")
            .uri(format!("/api/chat/session/{}/unblock", id))
            .header("content-type", "application/json")
            .body(Body::from(r#"{"resolution":"abort"}"#))
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);

        let raw = std::fs::read_to_string(&wf_path).unwrap();
        let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(
            v["tasks"]["align_stalled"]["state"]["status"].as_str(),
            Some("failed"),
            "abort resolution must mark the blocked task as failed"
        );
        assert!(
            v["tasks"]["align_stalled"]["state"]["reason"]
                .as_str()
                .unwrap_or("")
                .contains("aborted"),
            "failure reason should mention the abort path"
        );
        assert_eq!(
            v["tasks"]["not_blocked"]["state"]["status"].as_str(),
            Some("ready"),
        );
        drop(pkg);
    }

    // ── Path-jail ──────────────────────────────────────
    //
    // The stage-scoped confirm path formats `stage_id` (from the request
    // body, attacker-controlled) into the filename
    // `sme-review-confirmed-{stage_id}.json` and writes it inside the
    // package's runtime/ dir. Without the jail a request like
    // `{"stage": "../../tmp/escape"}` would write to
    // `<pkg>/runtime/sme-review-confirmed-../../tmp/escape.json`,
    // collapsing to a path outside the package.
    #[tokio::test]
    async fn post_confirm_rejects_traversal_stage_id() {
        let pkg = tempfile::TempDir::new().unwrap();
        let (router, app) = make_router(vec![]).await;
        let id =
            seed_session_with_completed_task(&app, "t_demo", Some(pkg.path().to_path_buf())).await;
        let req = Request::builder()
            .method("POST")
            .uri(format!("/api/chat/session/{}/confirm", id))
            .header("content-type", "application/json")
            .body(Body::from(r#"{"stage":"../../tmp/escape"}"#))
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::BAD_REQUEST,
            "traversal stage_id must be rejected with 400"
        );
        // Verify no sidecar was written outside the package.
        assert!(
            !pkg.path()
                .parent()
                .unwrap()
                .join("tmp/escape.json")
                .exists(),
            "no escape-path write should have happened"
        );
    }
}
