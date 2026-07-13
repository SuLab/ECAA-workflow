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
        // Imported (read-only) packages cannot be branched — no lineage
        // substrate to fork and no live session to re-emit from.
        if let Err(resp) = crate::chat_routes::package_import::ensure_not_imported(&s) {
            return resp;
        }
        if let super::IfMatchOutcome::Mismatch { server, client } =
            super::check_if_match(&headers, &s, "branch_session")
        {
            return super::precondition_failed_response(&server, &client);
        }
    }
    // `Idempotency-Key` short-circuit. A retry
    // within `ECAA_IDEMPOTENCY_TTL_SECS` with the same header value
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

    let (rationale, task_id, edits) = body
        .map(|BoundedJson(b)| (b.rationale, b.task_id, b.edits))
        .unwrap_or((None, None, None));

    // Guard the parent's state + the task boundary BEFORE forking. Load the
    // parent once (the endpoint's If-Match check reads it too, but that scope
    // has closed by here). A branch from a disallowed state or an unknown task
    // would otherwise silently produce a full-inherit branch masquerading as
    // task-scoped.
    let parent = match app.conversation.get_session(parent_id).await {
        Some(p) => p,
        None => return StatusCode::NOT_FOUND.into_response(),
    };
    // Disallowed source states: mid-intake (Greeting), mid-emit (Emitting), and
    // mid-amend (Amending) have no stable substrate to fork. `Blocked` is
    // allowed ONLY for a task-scoped branch — an SME forking a blocked task to
    // try an alternative is legitimate; a session-scoped branch of a blocked
    // session is not. Mirrors the tool-level guard in `tools/branch.rs`.
    match &parent.state {
        ecaa_workflow_conversation::SessionState::Greeting
        | ecaa_workflow_conversation::SessionState::Emitting
        | ecaa_workflow_conversation::SessionState::Amending { .. } => {
            return (
                StatusCode::CONFLICT,
                format!(
                    "cannot branch a session in state {:?}; wait for it to settle first",
                    parent.state
                ),
            )
                .into_response();
        }
        ecaa_workflow_conversation::SessionState::Blocked { .. } if task_id.is_none() => {
            return (
                StatusCode::CONFLICT,
                "a blocked session can only be branched from a specific task; \
                 pass task_id to fork the blocked task"
                    .to_string(),
            )
                .into_response();
        }
        _ => {}
    }
    // Task-membership guard: an unknown task_id is a 400, not a silent
    // full-inherit branch. `branch_from_at_task` now propagates the reset error;
    // validating here gives the SME a clean 400 with the offending id.
    if let Some(tid) = &task_id {
        let known = parent
            .current_dag()
            .map(|d| d.tasks.contains_key(tid.as_str()))
            .unwrap_or(false);
        if !known {
            return (
                StatusCode::BAD_REQUEST,
                format!("unknown task_id `{tid}`: not a member of the session DAG"),
            )
                .into_response();
        }
    }

    // Step 1: create the lineage record + save the child session. The
    // ConversationService handles (a) `Session::branch_from` in memory
    // and (b) persisting the child + parent decision-log update.
    // Rollback: delete the child session file (best-effort; the
    // session store has no expose-delete API so we log only).
    let child_id = match app
        .conversation
        .branch_session_with_rationale_and_task(parent_id, false, rationale, task_id.clone())
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
    // Branch-to-edit: apply the SME's staged method / parameter / bound edits
    // to the CHILD before it auto-emits, so the emitted child package carries
    // them. Applied here (not inside `branch_session_with_rationale_and_task`)
    // so the shared edit logic lives in the conversation crate and the branch's
    // minted confirmation token is preserved. A bad edit fails the branch with
    // a 400 — the child session is already persisted (unedited) and can be
    // retried, matching the fork-then-configure contract.
    if let Some(edits) = edits {
        if let Err(e) = app
            .conversation
            .apply_branch_edits_from_rest(
                child_id,
                task_id.clone(),
                edits.method,
                edits.parameters,
                edits.validation_bounds,
            )
            .await
        {
            let msg = format!("{e}");
            let cleaned = msg
                .strip_prefix("internal error: ")
                .unwrap_or(&msg)
                .to_string();
            return (StatusCode::BAD_REQUEST, cleaned).into_response();
        }
    }
    // A branch of an already-emitted parent no longer auto-emits: the
    // child is staged at ReadyToEmit and requires an EXPLICIT SME confirm
    // (`crates/conversation/src/service/transitions.rs`). The child's own
    // emit — and the artifact carry-over below — therefore run on the
    // post-confirm emit path (`chat_routes::turns::fire_auto_emit_postlogic`),
    // not here. See Task 5.1.

    // Surface which completed-task artifacts the branch would inherit but
    // are MISSING in the parent package.
    //
    // `branch_from` (in conversation/src/session/lineage.rs) copies the
    // parent's `task_states` map so inherited tasks land in the child's
    // DAG as Completed. The post-confirm emit writes the child's
    // WORKFLOW.json with those states intact and then carries over the
    // on-disk artifact directories (`runtime/outputs/<task_id>/...`,
    // `data/`) that the parent's harness wrote when those tasks ran for
    // real.
    //
    // Without that carry-over, the next harness against the child sees
    // Ready downstream tasks whose `depends_on` parents are nominally
    // Completed but materially empty: the dispatched compute agent finds
    // only `task-spec.json` in the upstream task dir, can't satisfy its
    // inputs, and either fabricates data (silent wrong answer) or
    // re-derives upstream work (defeats the point of branching from a
    // partial run). We hit the wrong-answer mode in the time-series
    // branch — child fit a different SARIMA on synthesized data and
    // validation correctly flagged the discrepancy.
    //
    // At branch time the child has not emitted yet (no
    // `emitted_package_path`), so `inherit_branch_artifacts` runs in
    // preview mode: it does not copy, it just reports which inherited
    // Completed tasks lack a materialized parent artifact dir. The SME
    // sees these as `artifacts_missing` before confirming; the real
    // hardlink/copy carry-over happens at the post-confirm emit.
    let artifacts_missing = inherit_branch_artifacts(&app, parent_id, child_id).await;

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
                    let cfg = app_for_git.commit_git_config();
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
            target: "ecaa::branch",
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
        // Additive: task ids whose inherited-Completed parent artifact dir
        // is missing (empty when all present). The UI keys a non-blocking
        // warning banner off this. Always an array so the consumer never
        // has to guard for null.
        "artifacts_missing": artifacts_missing,
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
/// `data/` dir) from the parent package into the child package, so a
/// downstream task dispatched in the branch sees real upstream outputs
/// instead of an empty task dir holding only `task-spec.json`.
///
/// Returns the list of inherited-Completed task ids whose parent artifact
/// dir is MISSING (skipped). Empty when every inherited prereq has a
/// materialized parent dir.
///
/// Two call sites, distinguished by whether the child has emitted yet:
/// - **Branch endpoint (preview).** The child is staged at `ReadyToEmit`
///   with no `emitted_package_path` — there is no package to copy INTO,
///   so this only reports the missing set for the `artifacts_missing`
///   branch response. The real carry-over runs later.
/// - **Post-confirm emit (copy).** Once the SME confirms and the child
///   emits its own package, this hardlinks (COW-friendly; copies on
///   cross-filesystem `EXDEV`) each inherited task dir and returns the
///   same missing set. Best-effort: missing parent files / IO errors are
///   logged at WARN and skipped, never aborted.
pub(crate) async fn inherit_branch_artifacts(
    app: &ChatAppState,
    parent_id: Uuid,
    child_id: Uuid,
) -> Vec<String> {
    let Some((_parent_session, parent_pkg)) = load_session_with_pkg(
        app,
        parent_id,
        "parent",
        "parent session not loadable; skipping artifact inheritance",
        "parent has no emitted_package_path; nothing to inherit",
    )
    .await else {
        return Vec::new();
    };
    let Some(child_session) = app.conversation.get_session(child_id).await else {
        tracing::warn!(
            target: "ecaa::branch::inherit",
            role = "child",
            session = %child_id,
            "child session not loadable; skipping artifact inheritance"
        );
        return Vec::new();
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

    // Missing set: inherited-Completed tasks that would inherit ZERO real
    // artifacts. The preview must mirror what the copy actually delivers, not
    // mere directory existence: `copy_or_hardlink_tree` skips any file the child
    // already has (its freshly-emitted `task-spec.json`), so an empty-but-present
    // parent task dir — or one holding only `task-spec.json` — delivers nothing.
    // Keying the preview on directory existence alone reported such a task as
    // present while the copy inherited zero files: the silent-wrong-answer this
    // feature exists to catch. Count real (non-`task-spec.json`) files instead.
    let parent_outputs_root = parent_pkg.join("runtime").join("outputs");
    let mut missing: Vec<String> = completed
        .iter()
        .filter(|tid| parent_task_real_artifact_count(&parent_outputs_root.join(tid.as_str())) == 0)
        .cloned()
        .collect();
    missing.sort();
    missing.dedup();

    // Real carry-over only once the child has emitted its own package
    // (post-confirm emit path). At branch time the child is `ReadyToEmit`
    // with no package; `missing` above is the preview and the copy runs
    // later, when the SME confirms and the child emits.
    if let Some(child_pkg) = child_session.emitted_package_path.clone() {
        let (tasks_inherited, mut files_inherited) =
            inherit_completed_task_dirs(parent_id, child_id, &parent_pkg, &child_pkg, &completed);
        files_inherited += inherit_parent_data_dir(parent_id, child_id, &parent_pkg, &child_pkg);
        let assumptions_inherited = inherit_completed_task_assumptions(
            parent_id,
            child_id,
            &parent_pkg,
            &child_pkg,
            &completed,
        );
        tracing::info!(
            target: "ecaa::branch::inherit",
            parent = %parent_id,
            child = %child_id,
            tasks = tasks_inherited,
            files = files_inherited,
            assumptions = assumptions_inherited,
            missing = missing.len(),
            "branch artifact inheritance complete"
        );
    }

    missing
}

/// Load a session and its emitted package path, logging the supplied reasons
/// (warn on missing session, debug on missing package) under a `role`-tagged
/// field and returning `None` when either is absent.
async fn load_session_with_pkg(
    app: &ChatAppState,
    id: Uuid,
    role: &str,
    missing_session_msg: &str,
    missing_pkg_msg: &str,
) -> Option<(
    ecaa_workflow_conversation::session::Session,
    std::path::PathBuf,
)> {
    let Some(session) = app.conversation.get_session(id).await else {
        tracing::warn!(
            target: "ecaa::branch::inherit",
            %role,
            session = %id,
            "{}",
            missing_session_msg
        );
        return None;
    };
    let Some(pkg) = session.emitted_package_path.clone() else {
        tracing::debug!(
            target: "ecaa::branch::inherit",
            %role,
            session = %id,
            "{}",
            missing_pkg_msg
        );
        return None;
    };
    Some((session, pkg))
}

/// Carry over each completed task's output dir from the parent package into the
/// child, rewriting inherited JSON paths. Returns (tasks_inherited, files).
fn inherit_completed_task_dirs(
    parent_id: Uuid,
    child_id: Uuid,
    parent_pkg: &std::path::Path,
    child_pkg: &std::path::Path,
    completed: &[String],
) -> (usize, usize) {
    let parent_outputs_root = parent_pkg.join("runtime").join("outputs");
    let child_outputs_root = child_pkg.join("runtime").join("outputs");

    let mut tasks_inherited = 0usize;
    let mut files_inherited = 0usize;

    for tid in completed {
        let parent_task_dir = parent_outputs_root.join(tid);
        if !parent_task_dir.exists() {
            continue;
        }
        let child_task_dir = child_outputs_root.join(tid);
        match copy_or_hardlink_tree(&parent_task_dir, &child_task_dir) {
            Ok(n) => {
                inherit_rewrite_task_json(
                    parent_id,
                    child_id,
                    tid,
                    &child_task_dir,
                    parent_pkg,
                    child_pkg,
                );
                if n > 0 {
                    tasks_inherited += 1;
                    files_inherited += n;
                }
            }
            Err(e) => {
                tracing::warn!(
                    target: "ecaa::branch::inherit",
                    parent = %parent_id,
                    child = %child_id,
                    task_id = %tid,
                    error = %e,
                    "carry-over of task artifact dir failed; downstream task in branch may fail with missing-input blocker"
                );
            }
        }
    }
    (tasks_inherited, files_inherited)
}

/// Best-effort rewrite of inherited JSON manifest paths from parent → child
/// package root, logging (but not aborting) on failure.
fn inherit_rewrite_task_json(
    parent_id: Uuid,
    child_id: Uuid,
    tid: &str,
    child_task_dir: &std::path::Path,
    parent_pkg: &std::path::Path,
    child_pkg: &std::path::Path,
) {
    if let Err(e) = rewrite_inherited_json_paths(child_task_dir, parent_pkg, child_pkg) {
        tracing::warn!(
            target: "ecaa::branch::inherit",
            parent = %parent_id,
            child = %child_id,
            task_id = %tid,
            error = %e,
            "carry-over JSON path rewrite failed; inherited manifests may point at the parent package"
        );
    }
}

/// Inherit the top-level `data/` dir from the parent package (SME source files
/// consumed by data_import / data staging). Returns the file count copied.
fn inherit_parent_data_dir(
    parent_id: Uuid,
    child_id: Uuid,
    parent_pkg: &std::path::Path,
    child_pkg: &std::path::Path,
) -> usize {
    // Also inherit the top-level `data/` dir from the parent. Many
    // archetypes (clinical_trial_analysis, time_series_forecast) place
    // SME-supplied source files under `data/`, consumed by data_import.
    // A branch typically wants the same source files; copying once
    // (or hardlinking) is far cheaper than re-staging or asking the
    // SME to re-register inputs.
    let parent_data = parent_pkg.join("data");
    if !parent_data.exists() {
        return 0;
    }
    let child_data = child_pkg.join("data");
    match copy_or_hardlink_tree(&parent_data, &child_data) {
        Ok(n) => n,
        Err(e) => {
            tracing::warn!(
                target: "ecaa::branch::inherit",
                parent = %parent_id,
                child = %child_id,
                error = %e,
                "carry-over of top-level data/ dir failed"
            );
            0
        }
    }
}

/// Carry over task-scoped runtime assumptions for inherited completed tasks.
/// Inherited output files need their corresponding `output_unused` and other
/// task-local F rows in the child package, otherwise post-branch ECAA
/// validation sees produced artifacts with no local audit explanation.
fn inherit_completed_task_assumptions(
    parent_id: Uuid,
    child_id: Uuid,
    parent_pkg: &std::path::Path,
    child_pkg: &std::path::Path,
    completed: &[String],
) -> usize {
    if completed.is_empty() {
        return 0;
    }
    let parent_file = parent_pkg.join("runtime").join("assumptions.jsonl");
    if !parent_file.exists() {
        return 0;
    }
    let child_file = child_pkg.join("runtime").join("assumptions.jsonl");
    let completed: std::collections::HashSet<&str> = completed.iter().map(String::as_str).collect();

    let mut existing_ids = std::collections::HashSet::new();
    if let Ok(text) = std::fs::read_to_string(&child_file) {
        for line in text.lines() {
            let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
                continue;
            };
            if let Some(id) = value.get("id").and_then(serde_json::Value::as_str) {
                existing_ids.insert(id.to_string());
            }
        }
    }

    let parent_prefix = parent_pkg.to_string_lossy().to_string();
    let child_prefix = child_pkg.to_string_lossy().to_string();
    let text = match std::fs::read_to_string(&parent_file) {
        Ok(text) => text,
        Err(e) => {
            tracing::warn!(
                target: "ecaa::branch::inherit",
                parent = %parent_id,
                child = %child_id,
                error = %e,
                "carry-over of runtime assumptions failed while reading parent sidecar"
            );
            return 0;
        }
    };

    let mut rows = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Ok(mut value) = serde_json::from_str::<serde_json::Value>(trimmed) else {
            continue;
        };
        let Some(task_id) = value.get("task_id").and_then(serde_json::Value::as_str) else {
            continue;
        };
        if !completed.contains(task_id) {
            continue;
        }
        if let Some(id) = value.get("id").and_then(serde_json::Value::as_str) {
            if !existing_ids.insert(id.to_string()) {
                continue;
            }
        }
        rewrite_json_string_prefixes(&mut value, &parent_prefix, &child_prefix);
        if let Ok(row) = serde_json::to_string(&value) {
            rows.push(row);
        }
    }

    if rows.is_empty() {
        return 0;
    }
    if let Some(parent) = child_file.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            tracing::warn!(
                target: "ecaa::branch::inherit",
                parent = %parent_id,
                child = %child_id,
                error = %e,
                "carry-over of runtime assumptions failed while creating child runtime dir"
            );
            return 0;
        }
    }

    let needs_leading_newline = std::fs::read(&child_file)
        .ok()
        .and_then(|bytes| bytes.last().copied())
        .map(|b| b != b'\n')
        .unwrap_or(false);

    let mut file = match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&child_file)
    {
        Ok(file) => file,
        Err(e) => {
            tracing::warn!(
                target: "ecaa::branch::inherit",
                parent = %parent_id,
                child = %child_id,
                error = %e,
                "carry-over of runtime assumptions failed while opening child sidecar"
            );
            return 0;
        }
    };
    if needs_leading_newline {
        if let Err(e) = std::io::Write::write_all(&mut file, b"\n") {
            tracing::warn!(
                target: "ecaa::branch::inherit",
                parent = %parent_id,
                child = %child_id,
                error = %e,
                "carry-over of runtime assumptions failed while separating JSONL rows"
            );
            return 0;
        }
    }
    use std::io::Write as _;
    let mut written = 0usize;
    for row in rows {
        if let Err(e) = writeln!(&mut file, "{row}") {
            tracing::warn!(
                target: "ecaa::branch::inherit",
                parent = %parent_id,
                child = %child_id,
                error = %e,
                "carry-over of runtime assumptions failed while appending child sidecar"
            );
            return written;
        }
        written += 1;
    }
    written
}

/// Recursively walk `src`, materializing every regular file at the
/// matching path under `dst`. Hardlinks where possible (cheap, atomic,
/// no double-cost on COW filesystems); falls back to byte copy on
/// `EXDEV` (cross-filesystem) or other hardlink errors. Returns the
/// count of files materialized. Errors propagate from the directory
/// walk; per-file errors are converted into copy-fallback attempts and
/// only surface when both link and copy fail.
/// Count the REAL artifacts a branch would inherit from a parent task dir:
/// every regular file EXCEPT `task-spec.json`. The child regenerates its own
/// `task-spec.json` at emit, and `copy_or_hardlink_tree` skips any file the
/// child already has — so `task-spec.json` never counts as an inherited
/// artifact. Returns 0 for a missing, empty, or spec-only directory, which is
/// exactly when the copy delivers nothing. Used by the `artifacts_missing`
/// preview so it predicts the copy's real delivery rather than mere existence.
fn parent_task_real_artifact_count(parent_task_dir: &std::path::Path) -> usize {
    if !parent_task_dir.is_dir() {
        return 0;
    }
    let mut count = 0usize;
    let mut stack = vec![parent_task_dir.to_path_buf()];
    while let Some(cur) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&cur) else {
            continue;
        };
        for entry in rd.flatten() {
            let Ok(ft) = entry.file_type() else {
                continue;
            };
            if ft.is_dir() {
                stack.push(entry.path());
            } else if ft.is_file() && entry.file_name() != "task-spec.json" {
                count += 1;
            }
        }
    }
    count
}

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
        assistant, augmented_config, body_json, make_router, make_router_with_config,
        seed_session_with_completed_task, tool_use,
    };
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use ecaa_workflow_conversation::{BatchableTool, SessionState, Tool};
    use tower::util::ServiceExt;
    use uuid::Uuid;

    const MIN_LFC_PARAM: &str = "parameters:\n  - name: min_lfc\n    type: number\n    description: \"SME log fold-change floor\"";

    /// Branch endpoint hardening: an unknown `task_id` is a 400, not a silent
    /// full-inherit branch.
    #[tokio::test(flavor = "multi_thread")]
    async fn branch_rejects_unknown_task_id() {
        let (router, app) = make_router(vec![]).await;
        let id = seed_session_with_completed_task(&app, "t_known", None).await;
        // Emitted so the state guard passes and the task-membership check runs.
        app.conversation
            .store_handle()
            .update(id, |s| {
                s.state = SessionState::Emitted;
                Ok(())
            })
            .await
            .unwrap();
        let resp = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/chat/session/{}/branch", id))
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"task_id":"no_such_task"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    /// Branch endpoint hardening: a session still in Greeting has no substrate
    /// to fork — reject with 409.
    #[tokio::test(flavor = "multi_thread")]
    async fn branch_rejects_greeting_state() {
        let (router, _app) = make_router(vec![]).await;
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
        // Fresh session is Greeting — the branch must be refused.
        let resp = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/chat/session/{}/branch", parent))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::CONFLICT);
    }

    /// Branch-to-edit: a task-scoped branch carrying `edits.parameters`
    /// produces a child whose branched task carries the SME override.
    #[tokio::test(flavor = "multi_thread")]
    async fn branch_with_edits_applies_parameter_override_to_child() {
        let cfg = augmented_config("differential_expression", MIN_LFC_PARAM);
        let (router, app) = make_router_with_config(
            cfg,
            vec![
                tool_use(Tool::Batchable(BatchableTool::AppendIntakeProse {
                    prose: "bulk rna-seq differential expression in human samples".into(),
                })),
                assistant("ok."),
            ],
        )
        .await;
        let (id, _) = app.conversation.start_session(false).await.unwrap();
        app.conversation
            .send_turn(id, "set it up".into(), None)
            .await
            .unwrap();
        let parent = app
            .conversation
            .store_handle()
            .update(id, |s| {
                // Emitted so the branch state guard passes; keep
                // emitted_package_path None so the child doesn't auto-emit to
                // disk (the edit-application is what we're asserting).
                s.state = SessionState::Emitted;
                s.emitted_package_path = None;
                s.ensure_dag_cached();
                Ok(())
            })
            .await
            .unwrap();
        let task_id = parent
            .dag
            .as_ref()
            .expect("composed dag")
            .tasks
            .iter()
            .find(|(_, t)| t.source_atom_id.as_deref() == Some("differential_expression"))
            .map(|(k, _)| k.to_string())
            .expect("a task backed by the differential_expression atom");

        let body = serde_json::json!({
            "task_id": task_id,
            "rationale": "branch to tighten the LFC floor",
            "edits": { "parameters": { "min_lfc": 2.0 } }
        })
        .to_string();
        let resp = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/chat/session/{}/branch", id))
                    .header("content-type", "application/json")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "branch-to-edit must succeed");
        let child_id = body_json(resp.into_body()).await["branched_session_id"]
            .as_str()
            .unwrap()
            .to_string();
        let child = app
            .conversation
            .get_session(Uuid::parse_str(&child_id).unwrap())
            .await
            .unwrap();
        let ov = child
            .sme_parameter_overrides
            .for_task(&task_id)
            .expect("child task must carry the branch-edit override");
        assert_eq!(
            ov.get("min_lfc").map(|o| &o.value),
            Some(&serde_json::json!(2.0)),
            "child override value must match the staged edit"
        );
        assert!(
            child.decisions.iter().any(|d| matches!(
                &d.decision,
                ecaa_workflow_core::decision_log::DecisionType::SetTaskParameter { parameter, .. }
                    if parameter == "min_lfc"
            )),
            "a SetTaskParameter decision must be recorded on the child"
        );
        // The branched task is still present in the child DAG (forward slice
        // was reset, not dropped).
        assert!(
            child.dag_contains_task(&task_id),
            "the branched task must remain in the child DAG"
        );
    }

    /// FIX 5: `apply_branch_edits` validates ALL edits before mutating. A branch
    /// carrying a valid method AND an invalid parameter must fail 400 and leave
    /// the child with NO method change (no `AmendStage` decision) — no phantom
    /// method edit committed ahead of the failed parameter validation.
    #[tokio::test(flavor = "multi_thread")]
    async fn branch_with_valid_method_and_invalid_param_leaves_no_phantom_method_edit() {
        let cfg = augmented_config("differential_expression", MIN_LFC_PARAM);
        let (router, app) = make_router_with_config(
            cfg,
            vec![
                tool_use(Tool::Batchable(BatchableTool::AppendIntakeProse {
                    prose: "bulk rna-seq differential expression in human samples".into(),
                })),
                assistant("ok."),
            ],
        )
        .await;
        let (id, _) = app.conversation.start_session(false).await.unwrap();
        app.conversation
            .send_turn(id, "set it up".into(), None)
            .await
            .unwrap();
        let parent = app
            .conversation
            .store_handle()
            .update(id, |s| {
                s.state = SessionState::Emitted;
                s.emitted_package_path = None;
                s.ensure_dag_cached();
                Ok(())
            })
            .await
            .unwrap();
        let task_id = parent
            .dag
            .as_ref()
            .expect("composed dag")
            .tasks
            .iter()
            .find(|(_, t)| t.source_atom_id.as_deref() == Some("differential_expression"))
            .map(|(k, _)| k.to_string())
            .expect("a task backed by the differential_expression atom");

        // Valid method + a type-mismatched min_lfc (declared `number`, sent a
        // string) — the parameter validation must reject the whole request.
        let body = serde_json::json!({
            "task_id": task_id,
            "edits": {
                "method": "use DESeq2 with apeglm shrinkage",
                "parameters": { "min_lfc": "not-a-number" }
            }
        })
        .to_string();
        let resp = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/chat/session/{}/branch", id))
                    .header("content-type", "application/json")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::BAD_REQUEST,
            "an invalid parameter must fail the branch edit with 400"
        );

        // The child was persisted (unedited) before the edit failed; it must
        // carry NO AmendStage decision (the phantom method edit the old order
        // would have committed before validating the bad parameter).
        let children = app.conversation.children_of(id).await;
        assert_eq!(children.len(), 1, "exactly one child must be persisted");
        let child = &children[0];
        assert!(
            !child.decisions.iter().any(|d| matches!(
                &d.decision,
                ecaa_workflow_core::decision_log::DecisionType::AmendStage { .. }
            )),
            "no AmendStage decision (phantom method edit) must be recorded on the child"
        );
        assert!(
            !child.intake_methods.0.contains_key(&task_id),
            "no method override must be committed for the task on the child"
        );
    }

    /// Task 5.4: when an inherited-Completed task's parent artifact dir is
    /// missing, the branch response surfaces its id in `artifacts_missing`
    /// so the SME sees the gap (the real carry-over runs at the child's
    /// confirmed emit).
    #[tokio::test(flavor = "multi_thread")]
    async fn branch_reports_missing_inherited_artifacts() {
        use ecaa_workflow_core::dag::TaskState;
        let pkg = tempfile::tempdir().unwrap();
        // Parent package exists but the completed task's output dir does NOT.
        std::fs::create_dir_all(pkg.path().join("runtime").join("outputs")).unwrap();

        let (router, app) = make_router(vec![]).await;
        let parent_id =
            seed_session_with_completed_task(&app, "t_done", Some(pkg.path().to_path_buf())).await;
        // Emitted so the branch state guard passes; task_states must carry
        // the Completed entry (the inherit path reads task_states, which the
        // seed helper leaves empty).
        app.conversation
            .store_handle()
            .update(parent_id, |s| {
                s.state = SessionState::Emitted;
                s.task_states.insert(
                    "t_done".to_string(),
                    TaskState::Completed {
                        result: serde_json::json!({}),
                    },
                );
                Ok(())
            })
            .await
            .unwrap();

        // Session-scoped branch (no task_id) so t_done stays Completed on
        // the child and is a carry-over candidate.
        let resp = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/chat/session/{}/branch", parent_id))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp.into_body()).await;
        let missing = body["artifacts_missing"]
            .as_array()
            .expect("artifacts_missing must be an array");
        assert!(
            missing.iter().any(|v| v == "t_done"),
            "branch response must report the missing inherited artifact, got {missing:?}"
        );
    }

    /// FIX 1 (branch leg): a branch of an ALREADY-EMITTED parent stages the
    /// child at PendingConfirmation; the SME's `/confirm` then drives the child
    /// all the way to Emitted (no illegal-transition 400) AND
    /// `fire_auto_emit_postlogic` carries over the parent's completed-task
    /// artifacts into the child package.
    #[tokio::test(flavor = "multi_thread")]
    #[serial_test::serial]
    async fn branch_of_emitted_parent_confirm_emits_child_and_inherits_artifacts() {
        use ecaa_workflow_core::dag::TaskState;

        let pkg_root = tempfile::tempdir().unwrap();
        let prev_root = std::env::var("ECAA_PACKAGE_ROOT").ok();
        std::env::set_var("ECAA_PACKAGE_ROOT", pkg_root.path());

        let (router, app) = make_router(vec![
            tool_use(Tool::Batchable(BatchableTool::AppendIntakeProse {
                prose: "bulk rna-seq differential expression in human samples".into(),
            })),
            assistant("ok."),
        ])
        .await;
        let (parent_id, _) = app.conversation.start_session(false).await.unwrap();
        app.conversation
            .send_turn(parent_id, "set it up".into(), None)
            .await
            .unwrap();

        // Emit the parent for real so it has a valid package to fork + diff from.
        let parent_dir = tempfile::tempdir().unwrap();
        let mut parent_session = app.conversation.get_session(parent_id).await.unwrap();
        ecaa_workflow_conversation::emit::emit_with_conversation_log(
            &mut parent_session,
            parent_dir.path(),
            &crate::chat_routes::test_support::config_dir(),
        )
        .await
        .expect("parent emit must succeed");

        // Pick a real task and mark it Completed with a materialized artifact.
        let task_id = parent_session
            .dag
            .as_ref()
            .expect("composed dag")
            .tasks
            .keys()
            .next()
            .expect("dag has tasks")
            .to_string();
        let task_out = parent_dir
            .path()
            .join("runtime")
            .join("outputs")
            .join(&task_id);
        std::fs::create_dir_all(&task_out).unwrap();
        std::fs::write(task_out.join("result.json"), r#"{"ok":true}"#).unwrap();

        app.conversation
            .store_handle()
            .update(parent_id, |s| {
                s.state = SessionState::Emitted;
                s.emitted_package_path = Some(parent_dir.path().to_path_buf());
                s.task_states.insert(
                    task_id.clone(),
                    TaskState::Completed {
                        result: serde_json::json!({}),
                    },
                );
                Ok(())
            })
            .await
            .unwrap();

        // Branch (session-scoped) — the child stages at PendingConfirmation.
        let resp = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/chat/session/{}/branch", parent_id))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let child_id = body_json(resp.into_body()).await["branched_session_id"]
            .as_str()
            .unwrap()
            .to_string();
        let child = app
            .conversation
            .get_session(Uuid::parse_str(&child_id).unwrap())
            .await
            .unwrap();
        assert!(
            matches!(child.state, SessionState::PendingConfirmation { .. }),
            "branch child must stage at PendingConfirmation, got {:?}",
            child.state
        );

        // The SME confirms the child → it must emit (no 400) + inherit artifacts.
        let resp = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/chat/session/{}/confirm", child_id))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::NO_CONTENT,
            "confirming the branch child must succeed (no illegal-transition 400)"
        );

        let child = app
            .conversation
            .get_session(Uuid::parse_str(&child_id).unwrap())
            .await
            .unwrap();
        assert!(
            matches!(child.state, SessionState::Emitted),
            "child must reach Emitted after confirm, got {:?}",
            child.state
        );
        let child_pkg = child
            .emitted_package_path
            .clone()
            .expect("child must have emitted a package");
        let inherited = child_pkg
            .join("runtime")
            .join("outputs")
            .join(&task_id)
            .join("result.json");
        assert!(
            inherited.is_file(),
            "child package must inherit the parent's completed-task artifact at {}",
            inherited.display()
        );

        match prev_root {
            Some(v) => std::env::set_var("ECAA_PACKAGE_ROOT", v),
            None => std::env::remove_var("ECAA_PACKAGE_ROOT"),
        }
    }

    /// FIX 7: the `artifacts_missing` preview must key on real files, not mere
    /// directory existence. An inherited-Completed task whose parent dir is
    /// present-but-empty (or holds only task-spec.json) delivers zero artifacts,
    /// so it must be reported missing; a task with a real artifact must not.
    #[tokio::test(flavor = "multi_thread")]
    async fn branch_reports_empty_parent_task_dir_as_missing() {
        use ecaa_workflow_core::dag::TaskState;
        let pkg = tempfile::tempdir().unwrap();
        let outputs = pkg.path().join("runtime").join("outputs");
        // t_empty: present-but-empty dir → delivers nothing.
        std::fs::create_dir_all(outputs.join("t_empty")).unwrap();
        // t_spec_only: dir with ONLY task-spec.json → delivers nothing.
        std::fs::create_dir_all(outputs.join("t_spec_only")).unwrap();
        std::fs::write(outputs.join("t_spec_only").join("task-spec.json"), "{}").unwrap();
        // t_real: dir with a real artifact → delivers one file.
        std::fs::create_dir_all(outputs.join("t_real")).unwrap();
        std::fs::write(outputs.join("t_real").join("result.json"), "{\"ok\":true}").unwrap();

        let (router, app) = make_router(vec![]).await;
        let parent_id =
            seed_session_with_completed_task(&app, "t_real", Some(pkg.path().to_path_buf())).await;
        app.conversation
            .store_handle()
            .update(parent_id, |s| {
                s.state = SessionState::Emitted;
                for tid in ["t_empty", "t_spec_only", "t_real"] {
                    s.task_states.insert(
                        tid.to_string(),
                        TaskState::Completed {
                            result: serde_json::json!({}),
                        },
                    );
                }
                Ok(())
            })
            .await
            .unwrap();

        let resp = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/chat/session/{}/branch", parent_id))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp.into_body()).await;
        let missing: Vec<String> = body["artifacts_missing"]
            .as_array()
            .expect("artifacts_missing array")
            .iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect();
        assert!(
            missing.contains(&"t_empty".to_string()),
            "empty parent task dir must be reported missing, got {missing:?}"
        );
        assert!(
            missing.contains(&"t_spec_only".to_string()),
            "spec-only parent task dir must be reported missing, got {missing:?}"
        );
        assert!(
            !missing.contains(&"t_real".to_string()),
            "a parent task dir with a real artifact must NOT be reported missing, got {missing:?}"
        );
    }

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

        // Advance past Greeting so the state guard permits the branch.
        app.conversation
            .store_handle()
            .update(Uuid::parse_str(&parent_id).unwrap(), |s| {
                s.state = ecaa_workflow_conversation::SessionState::Intake;
                Ok(())
            })
            .await
            .unwrap();

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
        // Force Emitted so the branch state guard passes; the seeded dag carries
        // the `t_demo` task the branch pins to.
        app.conversation
            .store_handle()
            .update(parent_id, |s| {
                s.state = ecaa_workflow_conversation::SessionState::Emitted;
                Ok(())
            })
            .await
            .unwrap();
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
        let (router, app) = make_router(vec![]).await;

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

        // Advance past Greeting so the branch state guard permits the fork.
        app.conversation
            .store_handle()
            .update(Uuid::parse_str(&parent).unwrap(), |s| {
                s.state = ecaa_workflow_conversation::SessionState::Intake;
                Ok(())
            })
            .await
            .unwrap();

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
            [0u8; 32],
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
        let (router, app) = make_router(vec![]).await;

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

        // Advance past Greeting so the state guard permits the branch.
        app.conversation
            .store_handle()
            .update(Uuid::parse_str(&parent_id).unwrap(), |s| {
                s.state = ecaa_workflow_conversation::SessionState::Intake;
                Ok(())
            })
            .await
            .unwrap();

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
    fn inherit_completed_task_assumptions_appends_task_rows_without_duplicates() {
        let tmp = tempfile::TempDir::new().unwrap();
        let parent_pkg = tmp.path().join("parent-pkg");
        let child_pkg = tmp.path().join("child-pkg");
        std::fs::create_dir_all(parent_pkg.join("runtime")).unwrap();
        std::fs::create_dir_all(child_pkg.join("runtime")).unwrap();
        let inherited_detail = parent_pkg
            .join("runtime/outputs/data_acquisition/result.tsv")
            .to_string_lossy()
            .to_string();
        let parent_rows = vec![
            serde_json::json!({
                "id": "assump:keep",
                "kind": "output_unused",
                "task_id": "data_acquisition",
                "detail": inherited_detail,
            }),
            serde_json::json!({
                "id": "assump:dupe",
                "kind": "output_unused",
                "task_id": "data_acquisition",
                "detail": "runtime/outputs/data_acquisition/dupe.tsv",
            }),
            serde_json::json!({
                "id": "assump:skip-downstream",
                "kind": "output_unused",
                "task_id": "differential_expression",
                "detail": "runtime/outputs/differential_expression/de.tsv",
            }),
            serde_json::json!({
                "id": "assump:skip-global",
                "kind": "registry_default",
                "detail": "not task scoped",
            }),
        ];
        let parent_jsonl = parent_rows
            .iter()
            .map(serde_json::Value::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        std::fs::write(
            parent_pkg.join("runtime/assumptions.jsonl"),
            format!("{parent_jsonl}\n"),
        )
        .unwrap();
        std::fs::write(
            child_pkg.join("runtime/assumptions.jsonl"),
            serde_json::json!({
                "id": "assump:dupe",
                "kind": "output_unused",
                "task_id": "data_acquisition",
                "detail": "child keeps existing row"
            })
            .to_string(),
        )
        .unwrap();

        let written = super::inherit_completed_task_assumptions(
            Uuid::new_v4(),
            Uuid::new_v4(),
            &parent_pkg,
            &child_pkg,
            &["data_acquisition".to_string()],
        );
        assert_eq!(
            written, 1,
            "only one non-duplicate completed-task row lands"
        );

        let child_text = std::fs::read_to_string(child_pkg.join("runtime/assumptions.jsonl"))
            .expect("child assumptions sidecar");
        let rows = child_text
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
            .collect::<Vec<_>>();
        let ids = rows
            .iter()
            .filter_map(|row| row.get("id").and_then(serde_json::Value::as_str))
            .collect::<Vec<_>>();
        assert_eq!(ids, vec!["assump:dupe", "assump:keep"]);
        assert!(
            child_text.contains(child_pkg.to_string_lossy().as_ref()),
            "inherited absolute paths must point at the child package"
        );
        assert!(
            !child_text.contains(parent_pkg.to_string_lossy().as_ref()),
            "inherited rows must not retain parent package paths"
        );
        assert!(!child_text.contains("skip-downstream"));
        assert!(!child_text.contains("skip-global"));
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
