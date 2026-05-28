//! Branch/fork endpoints. `POST /session/:id/branch` forks the current
//! session via `ConversationService::branch_session_with_rationale`.
//! `GET /sessions?parent=<uuid>` lists direct children of a parent;
//! the `SessionTree` UI sidebar consumes both.

use super::*;
use axum::{extract::State, http::StatusCode, response::IntoResponse, Json};
use ecaa_workflow_core::saga::{Saga, SagaStep};
use serde::Deserialize;
use uuid::Uuid;

/// Server endpoint that forks the parent session into a new branched
/// session via the ConversationService::branch_session helper. Returns
/// the new session id so the chat pane can route the user there.
#[tracing::instrument(skip(app, headers, body), fields(session_id = %parent_id))]
pub async fn branch_session_endpoint(
    State(app): State<ChatAppState>,
    Path(parent_id): Path<Uuid>,
    headers: axum::http::HeaderMap,
    body: Option<BoundedJson<CheckpointDecisionRequest>>,
) -> axum::response::Response {
    // R3.8: If-Match precondition. Optimistic-concurrency check on
    // the PARENT session — a branch from a stale view of the parent
    // forks downstream lineage from the wrong substrate. Run before
    // the idempotency short-circuit so a 412 isn't cached.
    if let Some(s) = app.conversation.get_session(parent_id).await {
        if let super::IfMatchOutcome::Mismatch { server, client } =
            super::check_if_match(&headers, &s, "branch_session")
        {
            return super::precondition_failed_response(&server, &client);
        }
    }
    // `Idempotency-Key` short-circuit. A retry
    // within `SWFC_IDEMPOTENCY_TTL_SECS` with the same header value
    // replays the cached response — prevents a flaky network from
    // forking the same session twice.
    let ticket = app
        .idempotency
        .lookup(parent_id, "branch_session", &headers);
    if let Some(replay) = ticket.cached_response() {
        return replay;
    }
    let response = branch_session_inner(app.clone(), parent_id, body).await;
    ticket.store(&app.idempotency, response).await
}

async fn branch_session_inner(
    app: ChatAppState,
    parent_id: Uuid,
    body: Option<BoundedJson<CheckpointDecisionRequest>>,
) -> axum::response::Response {
    // Forking a
    // session writes a new package (atom registry copy + session
    // store entry + git commit hook). Cap at 6/min per parent session
    // so a refresh loop can't churn out branches.
    if let Err(status) = LlmRateBuckets::check(
        &app.llm_buckets.branch,
        parent_id,
        app.llm_rate_limits.branch,
    ) {
        return (
            status,
            "rate limit exceeded: /branch capped at 6/min/session",
        )
            .into_response();
    }

    let (rationale, task_id) = body
        .map(|BoundedJson(b)| (b.rationale, b.task_id))
        .unwrap_or((None, None));

    // Step 1: create the lineage record + save the child session. The
    // ConversationService handles (a) `Session::branch_from` in memory
    // and (b) persisting the child + parent decision-log update.
    // Rollback: delete the child session file (best-effort; the
    // session store has no expose-delete API so we log only).
    let child_id = match app
        .conversation
        .branch_session_with_rationale_and_task(parent_id, false, rationale, task_id)
        .await
    {
        Ok(id) => id,
        Err(ecaa_workflow_conversation::ServiceError::SessionNotFound) => {
            return StatusCode::NOT_FOUND.into_response();
        }
        Err(e) => {
            return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
        }
    };
    if let Err(e) = app.conversation.try_auto_emit_after_confirm(child_id).await {
        tracing::warn!(
            target: "swfc::branch",
            parent = %parent_id,
            child = %child_id,
            error = %e,
            "branch child auto-emit failed; child session remains persisted"
        );
    }

    // Inherit completed-task artifacts from the parent package.
    //
    // `branch_from` (in conversation/src/session/lineage.rs) copies the
    // parent's `task_states` map so inherited tasks land in the child's
    // DAG as Completed. The emit path writes the child's WORKFLOW.json
    // with those states intact. What it does NOT do is carry over the
    // on-disk artifact directories (`runtime/outputs/<task_id>/...`,
    // `data/`) that the parent's harness wrote when those tasks ran
    // for real.
    //
    // Without that carry-over, the next harness against the child sees
    // Ready downstream tasks whose `depends_on` parents are nominally
    // Completed but materially empty: the dispatched compute agent
    // finds only `task-spec.json` in the upstream task dir, can't
    // satisfy its inputs, and either fabricates data (silent wrong
    // answer) or re-derives upstream work (defeats the point of
    // branching from a partial run). We hit the wrong-answer mode in
    // the time-series branch — child fit a different SARIMA on
    // synthesized data and validation correctly flagged the discrepancy.
    //
    // Hardlink each inherited file (cheap, atomic, COW-friendly). Fall
    // back to copy on cross-filesystem EXDEV. Best-effort: a missing
    // parent artifact is logged and skipped — the downstream agent will
    // surface its own blocker on the missing input. Skip if the child
    // emit didn't happen (no `emitted_package_path`).
    inherit_branch_artifacts(&app, parent_id, child_id).await;

    // Steps 2 & 3 are server-side post-branch actions that are
    // independent of the conversation service but must roll back if
    // either fails. Wrap in a Saga so partial failures leave a trace in
    // the log and the response carries the correct status code.
    //
    // Step 2: broadcast the SSE PackageAmended event to the parent's
    // subscribers. Rollback: not meaningful (SSE is fire-and-forget).
    //
    // Step 3: fire the git-commit hook via the bounded GitHookPool.
    // Per CLAUDE.md: git failures are fire-and-forget — they log but
    // never roll back the triggering operation. `forward_only` encodes
    // this: no rollback registered.

    // Clone the captures needed inside the `move ||` closures.
    let app_for_sse = app.clone();
    let app_for_git = app.clone();
    let child_id_for_git = child_id;

    // The Saga executes synchronously using Tokio's `block_in_place`
    // so we can drive async calls from within the `Fn() -> Result<()>`
    // step closures without spawning additional tasks.
    let saga_result = tokio::task::block_in_place(|| {
        Saga::new()
            .step(SagaStep::forward_only("broadcast_sse", move || {
                // Drive the async broadcast from within a sync closure
                // via `futures::executor::block_on`. The SSE broadcaster
                // holds a tokio `RwLock`; `block_in_place` keeps the
                // tokio runtime alive so the lock is accessible.
                tokio::runtime::Handle::current().block_on(async {
                    app_for_sse
                        .broadcast(
                            parent_id,
                            SsePayload::PackageAmended {
                                session_id: parent_id,
                                amended_stage: "(session_branched)".into(),
                                invalidated_tasks: vec![],
                                package_path: child_id.to_string(),
                            },
                        )
                        .await;
                    Ok(())
                })
            }))
            .step(SagaStep::forward_only("git_hook", move || {
                // git hook on branch. The service records the branch decision
                // on the parent session/package, and auto-emitted branches also
                // get their own child package. Commit both package repos in one
                // bounded hook so the parent audit trail does not stay dirty.
                tokio::runtime::Handle::current().block_on(async {
                    let parent_pkg = app_for_git
                        .conversation
                        .get_session(parent_id)
                        .await
                        .and_then(|s| s.emitted_package_path.clone());
                    let child_pkg = app_for_git
                        .conversation
                        .get_session(child_id_for_git)
                        .await
                        .and_then(|s| s.emitted_package_path.clone());
                    if parent_pkg.is_none() && child_pkg.is_none() {
                        return Ok(());
                    }
                    let cfg = app_for_git.git_config().read().clone();
                    let parent = parent_id.to_string();
                    let child = child_id_for_git.to_string();
                    let parent_short = parent[..8.min(parent.len())].to_string();
                    let child_short = child[..8.min(child.len())].to_string();
                    let app_for_drop = app_for_git.clone();
                    let drop_notifier: DropNotifier =
                        Arc::new(move |trigger: &str, reason: &str| {
                            app_for_drop.spawn_fanout(
                                child_id_for_git,
                                SsePayload::ProvenanceCommitDropped {
                                    trigger: trigger.to_string(),
                                    reason: reason.to_string(),
                                },
                            );
                        });
                    app_for_git.git_hook_pool.spawn_with_sink(
                        "branch",
                        move || {
                            if let Some(parent_pkg) = parent_pkg {
                                crate::git_routes::service::hook_commit(
                                    &cfg,
                                    &parent_pkg,
                                    "branch",
                                    &format!("to {}", child_short),
                                    &parent,
                                );
                            }
                            if let Some(child_pkg) = child_pkg {
                                crate::git_routes::service::hook_commit(
                                    &cfg,
                                    &child_pkg,
                                    "branch",
                                    &format!("from {} -> {}", parent_short, child_short),
                                    &parent,
                                );
                            }
                            Ok(())
                        },
                        Some(drop_notifier),
                    );
                    Ok(())
                })
            }))
            .execute()
    });

    if let Err(e) = saga_result {
        tracing::warn!(
            target: "swfc::branch",
            parent = %parent_id,
            child = %child_id,
            error = %e,
            "branch post-steps saga failed; child session is persisted"
        );
        // The child session was already saved in step 1. Return 200 with
        // the child id so the UI can navigate to the new session; the
        // SSE / git-hook failure is non-fatal per CLAUDE.md fire-and-forget
        // semantics.
    }

    Json(serde_json::json!({
        "branched_session_id": child_id,
        "session_id": child_id,
    }))
    .into_response()
}

/// List the N most-recently-active sessions across all roots and
/// branches. Drives the title-bar "Recent ▼" dropdown so SMEs can jump
/// back into a workflow they navigated away from. `?limit=` defaults to
/// 20 and is capped at 100.
///
/// `execution_status` is a separate field from `state_kind`: the former
/// reports whether a harness is *currently alive* for the session
/// (`running` / `exited` / `idle`), the latter reports the logical
/// session state (`emitted`, `blocked`, etc). The two are independent —
/// a session can be `Emitted` with `idle` execution (package was emitted
/// but no harness has been launched, or the previous harness exited), or
/// `Blocked` with `running` execution (mid-task SME approval). The UI
/// renders them as separate pills.
pub async fn list_recent_sessions(
    State(app): State<ChatAppState>,
    axum::extract::Query(params): axum::extract::Query<RecentQuery>,
) -> impl IntoResponse {
    let limit = params.limit.unwrap_or(20).clamp(1, 100);
    // Use the persistence-layer metadata cache instead of
    // `iter_sessions()`. The "Recent ▼" dropdown polls every 30s; the
    // cache avoids re-deserializing every session file per poll.
    let mut sessions = app.conversation.iter_session_metadata().await;
    sessions.sort_by_key(|m| std::cmp::Reverse(m.last_activity));
    // DashMap is lock-free at the shard level; no need to hold a guard
    // outside the iteration. The execution map is small (one entry per
    // active session) and the dropdown polls every 30 s.
    let summaries: Vec<serde_json::Value> = sessions
        .into_iter()
        .take(limit)
        .map(|m| {
            let parent_id = m.parent_session_id.map(|p| p.to_string());
            // `exit_status` is `Arc<AtomicI64>`; reader uses
            // `exit_status_get()` which loads with `Acquire` ordering
            // and converts the `EXIT_STATUS_UNSET` sentinel back to
            // `Option<i32>` for the existing match arm shape.
            let execution_status = match app
                .executions
                .get(&m.id)
                .map(|e| e.value().exit_status_get())
            {
                Some(None) => "running",
                Some(Some(_)) => "exited",
                None => "idle",
            };
            serde_json::json!({
                "session_id": m.id.to_string(),
                "title": m.title,
                "created_at": m.created_at,
                "last_activity": m.last_activity,
                "state_kind": m.state_kind,
                "execution_status": execution_status,
                "parent_id": parent_id,
                "n_turns": m.n_turns,
                "project_class": m.project_class,
            })
        })
        .collect();
    Json(summaries).into_response()
}

#[derive(Debug, Deserialize)]
pub struct RecentQuery {
    #[serde(default)]
    pub limit: Option<usize>,
}

/// List every session whose lineage points at `parent` (taken from
/// `?parent=<uuid>` query string).
pub async fn list_sessions_by_parent(
    State(app): State<ChatAppState>,
    axum::extract::Query(params): axum::extract::Query<ParentQuery>,
) -> impl IntoResponse {
    let Some(parent_str) = params.parent else {
        return (
            StatusCode::BAD_REQUEST,
            "missing required ?parent=<uuid> query parameter".to_string(),
        )
            .into_response();
    };
    let parent_id = match Uuid::parse_str(&parent_str) {
        Ok(id) => id,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                "parent id must be a UUID".to_string(),
            )
                .into_response()
        }
    };
    // Metadata-cache projection avoids the full Session-shape
    // deserialization the SessionTree sidebar would otherwise pay
    // for every render.
    let children = app.conversation.children_of_metadata(parent_id).await;
    let summaries: Vec<serde_json::Value> = children
        .into_iter()
        .map(|m| {
            let lineage_json = m.lineage_summary.as_ref().map(|l| {
                serde_json::json!({
                    "parent_session_id": l.parent_session_id.to_string(),
                    "branched_at": l.branched_at,
                    "branched_from_turn_index": l.branched_from_turn_index,
                })
            });
            serde_json::json!({
                "session_id": m.id.to_string(),
                "created_at": m.created_at,
                "lineage": lineage_json,
                "state_kind": m.state_kind,
            })
        })
        .collect();
    Json(summaries).into_response()
}

#[derive(Debug, Deserialize)]
pub struct ParentQuery {
    #[serde(default)]
    pub parent: Option<String>,
}

/// Carry over completed-task artifact directories (and the top-level
/// `data/` dir) from the parent package into the freshly-emitted
/// child package, so a downstream task dispatched in the branch sees
/// real upstream outputs instead of an empty task dir holding only
/// `task-spec.json`.
///
/// Returns the count of (task, files) hardlinked/copied. Best-effort:
/// missing parent files / cross-filesystem fallbacks / IO errors are
/// logged at WARN and skipped, never aborted — a branch is more
/// useful with partial inheritance than no inheritance.
async fn inherit_branch_artifacts(
    app: &ChatAppState,
    parent_id: Uuid,
    child_id: Uuid,
) -> (usize, usize) {
    let Some(child_session) = app.conversation.get_session(child_id).await else {
        tracing::warn!(
            target: "swfc::branch::inherit",
            child = %child_id,
            "child session not loadable; skipping artifact inheritance"
        );
        return (0, 0);
    };
    let Some(child_pkg) = child_session.emitted_package_path.clone() else {
        tracing::debug!(
            target: "swfc::branch::inherit",
            child = %child_id,
            "child has no emitted_package_path (intake-phase branch); nothing to inherit"
        );
        return (0, 0);
    };
    let Some(parent_session) = app.conversation.get_session(parent_id).await else {
        tracing::warn!(
            target: "swfc::branch::inherit",
            parent = %parent_id,
            "parent session not loadable; skipping artifact inheritance"
        );
        return (0, 0);
    };
    let Some(parent_pkg) = parent_session.emitted_package_path.clone() else {
        tracing::debug!(
            target: "swfc::branch::inherit",
            parent = %parent_id,
            "parent has no emitted_package_path; nothing to inherit"
        );
        return (0, 0);
    };

    // Collect the set of task ids the child considers Completed —
    // these are the inherited prereqs whose artifacts the branch needs.
    use ecaa_workflow_core::dag::TaskState;
    let completed: Vec<String> = child_session
        .task_states
        .iter()
        .filter_map(|(tid, st)| {
            if matches!(st, TaskState::Completed { .. }) {
                Some(tid.to_string())
            } else {
                None
            }
        })
        .collect();
    drop(child_session);
    drop(parent_session);

    let parent_outputs_root = parent_pkg.join("runtime").join("outputs");
    let child_outputs_root = child_pkg.join("runtime").join("outputs");

    let mut tasks_inherited = 0usize;
    let mut files_inherited = 0usize;

    for tid in &completed {
        let parent_task_dir = parent_outputs_root.join(tid);
        if !parent_task_dir.exists() {
            continue;
        }
        let child_task_dir = child_outputs_root.join(tid);
        match copy_or_hardlink_tree(&parent_task_dir, &child_task_dir) {
            Ok(n) => {
                if let Err(e) =
                    rewrite_inherited_json_paths(&child_task_dir, &parent_pkg, &child_pkg)
                {
                    tracing::warn!(
                        target: "swfc::branch::inherit",
                        parent = %parent_id,
                        child = %child_id,
                        task_id = %tid,
                        error = %e,
                        "carry-over JSON path rewrite failed; inherited manifests may point at the parent package"
                    );
                }
                if n > 0 {
                    tasks_inherited += 1;
                    files_inherited += n;
                }
            }
            Err(e) => {
                tracing::warn!(
                    target: "swfc::branch::inherit",
                    parent = %parent_id,
                    child = %child_id,
                    task_id = %tid,
                    error = %e,
                    "carry-over of task artifact dir failed; downstream task in branch may fail with missing-input blocker"
                );
            }
        }
    }

    // Also inherit the top-level `data/` dir from the parent. Many
    // archetypes (clinical_trial_analysis, time_series_forecast) place
    // SME-supplied source files under `data/`, consumed by data_import.
    // A branch typically wants the same source files; copying once
    // (or hardlinking) is far cheaper than re-staging or asking the
    // SME to re-register inputs.
    let parent_data = parent_pkg.join("data");
    if parent_data.exists() {
        let child_data = child_pkg.join("data");
        match copy_or_hardlink_tree(&parent_data, &child_data) {
            Ok(n) => files_inherited += n,
            Err(e) => {
                tracing::warn!(
                    target: "swfc::branch::inherit",
                    parent = %parent_id,
                    child = %child_id,
                    error = %e,
                    "carry-over of top-level data/ dir failed"
                );
            }
        }
    }

    tracing::info!(
        target: "swfc::branch::inherit",
        parent = %parent_id,
        child = %child_id,
        tasks = tasks_inherited,
        files = files_inherited,
        "branch artifact inheritance complete"
    );
    (tasks_inherited, files_inherited)
}

/// Recursively walk `src`, materializing every regular file at the
/// matching path under `dst`. Hardlinks where possible (cheap, atomic,
/// no double-cost on COW filesystems); falls back to byte copy on
/// `EXDEV` (cross-filesystem) or other hardlink errors. Returns the
/// count of files materialized. Errors propagate from the directory
/// walk; per-file errors are converted into copy-fallback attempts and
/// only surface when both link and copy fail.
fn copy_or_hardlink_tree(src: &std::path::Path, dst: &std::path::Path) -> std::io::Result<usize> {
    if !src.exists() {
        return Ok(0);
    }
    let metadata = std::fs::metadata(src)?;
    if !metadata.is_dir() {
        return Ok(0);
    }
    std::fs::create_dir_all(dst)?;
    let mut count = 0usize;
    let mut stack: Vec<std::path::PathBuf> = vec![src.to_path_buf()];
    while let Some(cur) = stack.pop() {
        for entry in std::fs::read_dir(&cur)? {
            let entry = entry?;
            let file_type = entry.file_type()?;
            let entry_path = entry.path();
            let rel = match entry_path.strip_prefix(src) {
                Ok(p) => p,
                Err(_) => continue,
            };
            let target = dst.join(rel);
            if file_type.is_dir() {
                std::fs::create_dir_all(&target)?;
                stack.push(entry_path);
            } else if file_type.is_file() {
                // Skip if the destination already exists — earlier
                // tasks in the branch may have written there, and
                // overwriting would clobber the branch's own state.
                if target.exists() {
                    continue;
                }
                if let Some(parent) = target.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                match std::fs::hard_link(&entry_path, &target) {
                    Ok(()) => {}
                    Err(_) => {
                        std::fs::copy(&entry_path, &target)?;
                    }
                }
                count += 1;
            }
            // Skip symlinks intentionally — we don't want to follow
            // them into anything outside the parent package root.
        }
    }
    Ok(count)
}

fn rewrite_inherited_json_paths(
    root: &std::path::Path,
    parent_pkg: &std::path::Path,
    child_pkg: &std::path::Path,
) -> std::io::Result<usize> {
    if !root.exists() {
        return Ok(0);
    }
    let parent_prefix = parent_pkg.to_string_lossy().to_string();
    let child_prefix = child_pkg.to_string_lossy().to_string();
    if parent_prefix == child_prefix {
        return Ok(0);
    }

    let mut rewritten = 0usize;
    let mut stack = vec![root.to_path_buf()];
    while let Some(cur) = stack.pop() {
        for entry in std::fs::read_dir(&cur)? {
            let entry = entry?;
            let file_type = entry.file_type()?;
            let path = entry.path();
            if file_type.is_dir() {
                stack.push(path);
                continue;
            }
            if !file_type.is_file() {
                continue;
            }
            let is_json = path
                .extension()
                .and_then(|ext| ext.to_str())
                .map(|ext| ext.eq_ignore_ascii_case("json"))
                .unwrap_or(false);
            if !is_json {
                continue;
            }

            let bytes = std::fs::read(&path)?;
            let Ok(mut value) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
                continue;
            };
            if !rewrite_json_string_prefixes(&mut value, &parent_prefix, &child_prefix) {
                continue;
            }

            let mut out = serde_json::to_vec_pretty(&value)?;
            out.push(b'\n');
            let tmp_name = path
                .file_name()
                .map(|name| format!("{}.rewrite.tmp", name.to_string_lossy()))
                .unwrap_or_else(|| ".rewrite.tmp".to_string());
            let tmp_path = path.with_file_name(tmp_name);
            std::fs::write(&tmp_path, out)?;
            std::fs::rename(&tmp_path, &path)?;
            rewritten += 1;
        }
    }

    Ok(rewritten)
}

fn rewrite_json_string_prefixes(
    value: &mut serde_json::Value,
    parent_prefix: &str,
    child_prefix: &str,
) -> bool {
    match value {
        serde_json::Value::String(s) => {
            if let Some(suffix) = s.strip_prefix(parent_prefix) {
                *s = format!("{child_prefix}{suffix}");
                true
            } else {
                false
            }
        }
        serde_json::Value::Array(items) => {
            let mut changed = false;
            for item in items {
                changed |= rewrite_json_string_prefixes(item, parent_prefix, child_prefix);
            }
            changed
        }
        serde_json::Value::Object(map) => {
            let mut changed = false;
            for item in map.values_mut() {
                changed |= rewrite_json_string_prefixes(item, parent_prefix, child_prefix);
            }
            changed
        }
        _ => false,
    }
}

/// Route inventory for the doc-as-contract gate +
/// per-submodule `routes()` builder. `mod.rs::router()` merges every
/// submodule's builder into the single chat surface.
pub(super) const ROUTES: &[(&str, &str)] = &[
    ("POST", "/api/chat/session/:id/branch"),
    ("GET", "/api/chat/sessions"),
    ("GET", "/api/chat/sessions/recent"),
];

pub(super) fn routes() -> axum::Router<ChatAppState> {
    axum::Router::new()
        .route(
            "/api/chat/session/:id/branch",
            axum::routing::post(branch_session_endpoint),
        )
        .route(
            "/api/chat/sessions",
            axum::routing::get(list_sessions_by_parent),
        )
        .route(
            "/api/chat/sessions/recent",
            axum::routing::get(list_recent_sessions),
        )
}

#[cfg(test)]
mod tests {
    use crate::chat_routes::test_support::{
        body_json, make_router, seed_session_with_completed_task,
    };
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::util::ServiceExt;
    use uuid::Uuid;

    #[tokio::test(flavor = "multi_thread")]
    async fn branch_endpoint_forks_session_and_returns_new_id() {
        let (router, app) = make_router(vec![]).await;
        // Create a parent session.
        let req = Request::builder()
            .method("POST")
            .uri("/api/chat/session")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"careful_mode": false}"#))
            .unwrap();
        let resp = router.clone().oneshot(req).await.unwrap();
        let parent_id = body_json(resp.into_body()).await["session_id"]
            .as_str()
            .unwrap()
            .to_string();

        // Branch from it.
        let req = Request::builder()
            .method("POST")
            .uri(format!("/api/chat/session/{}/branch", parent_id))
            .body(Body::empty())
            .unwrap();
        let resp = router.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp.into_body()).await;
        let child_id = body["branched_session_id"].as_str().unwrap();
        assert_ne!(child_id, parent_id, "branch must allocate a new id");

        // The child session must be persisted with lineage pointing
        // back at the parent.
        let child_uuid = Uuid::parse_str(child_id).unwrap();
        let child = app.conversation.get_session(child_uuid).await.unwrap();
        let lineage = child.lineage.expect("branch must record lineage");
        assert_eq!(lineage.parent_session_id.to_string(), parent_id);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn branch_endpoint_commits_parent_branch_decision() {
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

        std::fs::create_dir_all(pkg.path().join("runtime")).unwrap();
        std::fs::write(pkg.path().join("WORKFLOW.json"), "{}\n").unwrap();
        std::fs::write(pkg.path().join("runtime/decisions.jsonl"), "").unwrap();
        git(pkg.path(), &["init"]);
        git(pkg.path(), &["config", "user.name", "Test"]);
        git(pkg.path(), &["config", "user.email", "test@example.com"]);
        git(
            pkg.path(),
            &["add", "WORKFLOW.json", "runtime/decisions.jsonl"],
        );
        git(pkg.path(), &["commit", "-m", "emit: seed package"]);

        let parent_id =
            seed_session_with_completed_task(&app, "t_demo", Some(pkg.path().to_path_buf())).await;
        let req = Request::builder()
            .method("POST")
            .uri(format!("/api/chat/session/{}/branch", parent_id))
            .header("content-type", "application/json")
            .body(Body::from(
                r#"{"rationale":"audit parent branch decision","task_id":"t_demo"}"#,
            ))
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let mut clean = false;
        for _ in 0..60 {
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
            "branch git hook did not clean the parent package repo"
        );

        let head = git(pkg.path(), &["show", "HEAD:runtime/decisions.jsonl"]);
        assert!(
            head.contains(r#""kind":"branch""#),
            "parent branch decision was not committed in HEAD:\n{}",
            head
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn branch_endpoint_unknown_parent_is_404() {
        let (router, _) = make_router(vec![]).await;
        let bogus = Uuid::new_v4();
        let req = Request::builder()
            .method("POST")
            .uri(format!("/api/chat/session/{}/branch", bogus))
            .body(Body::empty())
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn list_sessions_by_parent_returns_only_children() {
        let (router, _) = make_router(vec![]).await;

        // Create two sessions; branch the first into two children;
        // leave the second alone. The query must surface only the
        // first's children.
        let req = Request::builder()
            .method("POST")
            .uri("/api/chat/session")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"careful_mode": false}"#))
            .unwrap();
        let resp = router.clone().oneshot(req).await.unwrap();
        let parent = body_json(resp.into_body()).await["session_id"]
            .as_str()
            .unwrap()
            .to_string();

        let req = Request::builder()
            .method("POST")
            .uri("/api/chat/session")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"careful_mode": false}"#))
            .unwrap();
        let _ = router.clone().oneshot(req).await.unwrap();

        for _ in 0..2 {
            let req = Request::builder()
                .method("POST")
                .uri(format!("/api/chat/session/{}/branch", parent))
                .body(Body::empty())
                .unwrap();
            let resp = router.clone().oneshot(req).await.unwrap();
            assert_eq!(resp.status(), StatusCode::OK);
        }

        let req = Request::builder()
            .method("GET")
            .uri(format!("/api/chat/sessions?parent={}", parent))
            .body(Body::empty())
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp.into_body()).await;
        let arr = body.as_array().expect("array");
        assert_eq!(arr.len(), 2, "must return exactly 2 children");
        for entry in arr {
            assert!(entry["lineage"]["parent_session_id"]
                .as_str()
                .unwrap()
                .contains(&parent));
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn recent_sessions_returns_all_sessions_newest_first() {
        let (router, _) = make_router(vec![]).await;
        // Create 3 sessions in order — last_activity advances on create.
        let mut ids = Vec::new();
        for _ in 0..3 {
            let req = Request::builder()
                .method("POST")
                .uri("/api/chat/session")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"careful_mode": false}"#))
                .unwrap();
            let resp = router.clone().oneshot(req).await.unwrap();
            assert_eq!(resp.status(), StatusCode::OK);
            let body = body_json(resp.into_body()).await;
            ids.push(body["session_id"].as_str().unwrap().to_string());
            // Microsecond gap so last_activity timestamps order.
            tokio::time::sleep(std::time::Duration::from_millis(2)).await;
        }

        let req = Request::builder()
            .method("GET")
            .uri("/api/chat/sessions/recent")
            .body(Body::empty())
            .unwrap();
        let resp = router.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp.into_body()).await;
        let arr = body.as_array().expect("array");
        assert!(arr.len() >= 3, "must include all 3 created");
        let returned: Vec<&str> = arr
            .iter()
            .take(3)
            .map(|v| v["session_id"].as_str().unwrap())
            .collect();
        // Newest first → reverse of creation order.
        assert_eq!(
            returned,
            vec![ids[2].as_str(), ids[1].as_str(), ids[0].as_str()]
        );
        for entry in arr {
            assert!(entry["state_kind"].is_string());
            assert!(entry["n_turns"].is_number());
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn recent_sessions_respects_limit() {
        let (router, _) = make_router(vec![]).await;
        for _ in 0..5 {
            let req = Request::builder()
                .method("POST")
                .uri("/api/chat/session")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"careful_mode": false}"#))
                .unwrap();
            let _ = router.clone().oneshot(req).await.unwrap();
        }
        let req = Request::builder()
            .method("GET")
            .uri("/api/chat/sessions/recent?limit=2")
            .body(Body::empty())
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let arr = body_json(resp.into_body()).await;
        assert_eq!(arr.as_array().unwrap().len(), 2);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn recent_sessions_surfaces_execution_status() {
        use crate::chat_routes::ExecutionHandle;

        let (router, app) = make_router(vec![]).await;

        // Create 3 sessions; we'll attach execution handles to the
        // first two and leave the third bare to verify all three
        // execution_status branches.
        let mut ids: Vec<String> = Vec::new();
        for _ in 0..3 {
            let req = Request::builder()
                .method("POST")
                .uri("/api/chat/session")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"careful_mode": false}"#))
                .unwrap();
            let resp = router.clone().oneshot(req).await.unwrap();
            ids.push(
                body_json(resp.into_body()).await["session_id"]
                    .as_str()
                    .unwrap()
                    .to_string(),
            );
            tokio::time::sleep(std::time::Duration::from_millis(2)).await;
        }
        let ids: Vec<Uuid> = ids.iter().map(|s| Uuid::parse_str(s).unwrap()).collect();

        // Attach a "running" handle (exit_status = None) to ids[0]
        // and an "exited" handle (exit_status = Some(0)) to ids[1].
        // ids[2] stays bare → "idle".
        // `ExecutionHandle::for_running` /
        // `for_exited` constructors hide the boilerplate.
        let running = ExecutionHandle::for_running(
            12345,
            12345,
            std::path::PathBuf::from("/tmp/fake-pkg"),
            "/bin/true".to_string(),
        );
        let exited = ExecutionHandle::for_exited(12346, 12346, 0);
        {
            app.executions.insert(ids[0], running);
            app.executions.insert(ids[1], exited);
        }

        let req = Request::builder()
            .method("GET")
            .uri("/api/chat/sessions/recent")
            .body(Body::empty())
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp.into_body()).await;
        let arr = body.as_array().expect("array");

        let by_id: std::collections::HashMap<&str, &serde_json::Value> = arr
            .iter()
            .map(|v| (v["session_id"].as_str().unwrap(), v))
            .collect();

        // Pre-condition: every entry surfaces the new field.
        for entry in arr {
            assert!(
                entry.get("execution_status").is_some(),
                "every recent-session row must carry execution_status; got {entry:?}"
            );
        }

        let id0 = ids[0].to_string();
        let id1 = ids[1].to_string();
        let id2 = ids[2].to_string();
        assert_eq!(
            by_id[id0.as_str()]["execution_status"].as_str().unwrap(),
            "running",
            "session with live (exit_status=None) handle must report running",
        );
        assert_eq!(
            by_id[id1.as_str()]["execution_status"].as_str().unwrap(),
            "exited",
            "session with reaped (exit_status=Some(_)) handle must report exited",
        );
        assert_eq!(
            by_id[id2.as_str()]["execution_status"].as_str().unwrap(),
            "idle",
            "session with no execution handle must report idle (not running)",
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn list_sessions_missing_parent_query_is_400() {
        let (router, _) = make_router(vec![]).await;
        let req = Request::builder()
            .method("GET")
            .uri("/api/chat/sessions")
            .body(Body::empty())
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    /// Saga wiring: `branch_session_inner` must produce 200 with a valid
    /// `branched_session_id` when the underlying `branch_session_with_rationale`
    /// succeeds. The Saga's two post-steps (SSE broadcast + git hook) are
    /// fire-and-forget and must not cause a failure response.
    #[tokio::test(flavor = "multi_thread")]
    async fn branch_endpoint_saga_returns_child_id_on_success() {
        let (router, _app) = make_router(vec![]).await;

        // Create a parent session.
        let req = Request::builder()
            .method("POST")
            .uri("/api/chat/session")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"careful_mode": false}"#))
            .unwrap();
        let resp = router.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let parent_id = body_json(resp.into_body()).await["session_id"]
            .as_str()
            .unwrap()
            .to_string();

        // Branch via the Saga-wrapped endpoint.
        let req = Request::builder()
            .method("POST")
            .uri(format!("/api/chat/session/{}/branch", parent_id))
            .body(Body::empty())
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        // Must be 200 even when there is no emitted package (git hook skips).
        assert_eq!(
            resp.status(),
            StatusCode::OK,
            "branch_session_inner must return 200 after Saga completes"
        );
        let body = body_json(resp.into_body()).await;
        let child_id = body["branched_session_id"].as_str().unwrap();
        assert_ne!(
            child_id, parent_id,
            "child session id must differ from parent"
        );
        // The id must be a valid UUID.
        Uuid::parse_str(child_id).expect("branched_session_id must be a valid UUID");
    }

    // ── inherit_branch_artifacts helper: copy_or_hardlink_tree ────────────
    //
    // The full end-to-end inheritance path requires two emitted packages
    // plus the branch handler's session-cloning machinery (covered by the
    // playwright e2e in the time-series branching scenario). These tests
    // pin the byte-level contract of the tree-copy helper that does the
    // actual work — easier to reason about and faster than a fresh full
    // session for every assertion.

    #[test]
    fn copy_or_hardlink_tree_creates_dst_and_copies_files() {
        let tmp = tempfile::TempDir::new().unwrap();
        let src = tmp.path().join("src/runtime/outputs/task_a");
        let dst = tmp.path().join("dst/runtime/outputs/task_a");
        std::fs::create_dir_all(src.join("figures")).unwrap();
        std::fs::write(src.join("result.json"), b"{\"k\":1}").unwrap();
        std::fs::write(src.join("figures/plot.png"), b"\x89PNG_fake").unwrap();
        std::fs::write(src.join("env.lock"), b"r-version=4.4.1").unwrap();

        let n = super::copy_or_hardlink_tree(&src, &dst).unwrap();
        assert_eq!(n, 3, "should materialize 3 files");
        assert_eq!(
            std::fs::read(dst.join("result.json")).unwrap(),
            b"{\"k\":1}"
        );
        assert_eq!(
            std::fs::read(dst.join("figures/plot.png")).unwrap(),
            b"\x89PNG_fake"
        );
        assert_eq!(
            std::fs::read(dst.join("env.lock")).unwrap(),
            b"r-version=4.4.1"
        );
    }

    #[test]
    fn copy_or_hardlink_tree_no_op_on_missing_src() {
        let tmp = tempfile::TempDir::new().unwrap();
        let src = tmp.path().join("does/not/exist");
        let dst = tmp.path().join("dst");
        let n = super::copy_or_hardlink_tree(&src, &dst).unwrap();
        assert_eq!(n, 0);
        assert!(!dst.exists(), "dst must not be created when src missing");
    }

    #[test]
    fn copy_or_hardlink_tree_preserves_existing_dst_files() {
        // The branch may have already written a file at the same path
        // (e.g. a freshly emitted task-spec.json). The carry-over must
        // not clobber branch-local state with the parent's version.
        let tmp = tempfile::TempDir::new().unwrap();
        let src = tmp.path().join("parent");
        let dst = tmp.path().join("child");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::create_dir_all(&dst).unwrap();
        std::fs::write(src.join("conflict.txt"), b"parent_value").unwrap();
        std::fs::write(src.join("new.txt"), b"parent_new").unwrap();
        std::fs::write(dst.join("conflict.txt"), b"child_value").unwrap();

        let n = super::copy_or_hardlink_tree(&src, &dst).unwrap();
        // Only `new.txt` should land; `conflict.txt` keeps the child's value.
        assert_eq!(n, 1);
        assert_eq!(
            std::fs::read(dst.join("conflict.txt")).unwrap(),
            b"child_value",
            "child-local file must not be clobbered by parent carry-over"
        );
        assert_eq!(std::fs::read(dst.join("new.txt")).unwrap(), b"parent_new");
    }

    #[test]
    fn inherited_json_paths_are_repointed_without_mutating_parent() {
        let tmp = tempfile::TempDir::new().unwrap();
        let parent_pkg = tmp.path().join("parent-pkg");
        let child_pkg = tmp.path().join("child-pkg");
        let parent_task = parent_pkg.join("runtime/outputs/data_acquisition");
        let child_task = child_pkg.join("runtime/outputs/data_acquisition");
        std::fs::create_dir_all(parent_task.join("figures")).unwrap();
        let parent_png = parent_task.join("figures/samples_per_study.png");
        std::fs::write(&parent_png, b"\x89PNG_fake").unwrap();
        std::fs::write(
            parent_task.join("figures/manifest.json"),
            serde_json::to_vec_pretty(&serde_json::json!({
                "stage_id": "data_acquisition",
                "written": {
                    "samples_per_study": parent_png.to_string_lossy(),
                },
                "formats": {
                    "samples_per_study": [
                        parent_png.to_string_lossy(),
                        parent_task.join("figures/samples_per_study.pdf").to_string_lossy(),
                    ],
                },
            }))
            .unwrap(),
        )
        .unwrap();

        super::copy_or_hardlink_tree(&parent_task, &child_task).unwrap();
        let rewritten =
            super::rewrite_inherited_json_paths(&child_task, &parent_pkg, &child_pkg).unwrap();
        assert_eq!(
            rewritten, 1,
            "only manifest.json should need path rewriting"
        );

        let child_manifest =
            std::fs::read_to_string(child_task.join("figures/manifest.json")).unwrap();
        assert!(
            child_manifest.contains(child_pkg.to_string_lossy().as_ref()),
            "child manifest must point at the child package"
        );
        assert!(
            !child_manifest.contains(parent_pkg.to_string_lossy().as_ref()),
            "child manifest must not retain parent-package absolute paths"
        );

        let parent_manifest =
            std::fs::read_to_string(parent_task.join("figures/manifest.json")).unwrap();
        assert!(
            parent_manifest.contains(parent_pkg.to_string_lossy().as_ref()),
            "rewriting the hardlinked child manifest must not mutate the parent manifest"
        );
        assert!(
            !parent_manifest.contains(child_pkg.to_string_lossy().as_ref()),
            "parent manifest must not be repointed to the child package"
        );
    }
}
