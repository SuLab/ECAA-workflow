//! Session-lifecycle handlers. `create_session` (new chat session +
//! greeting), `get_state` (SessionStateSnapshot for the right-pane
//! header), `get_transcript` (full conversation history), and
//! `get_metrics` (per-session telemetry polled by the Metrics tab).

use super::*;
use crate::auth::RequestPrincipal;
use axum::{extract::State, http::StatusCode, response::IntoResponse, Extension, Json};
use uuid::Uuid;

/// Persisted harness progress backlog for this session. Used by the
/// Progress tab on mount to hydrate the UI before the live SSE stream
/// picks up additional events.
///
/// Payload shape matches the SSE `harness_progress` event so the UI can
/// merge the two streams without shape conversion:
/// `{ kind, taskId, status, detail, remote, timestamp }`.
///
/// Supports `?cursor=<opaque>&limit=<n>` (default
/// 100, max 1000). The legacy unpaginated `events` array is preserved
/// alongside the new paginated `data`/`next_cursor`/`has_more` fields
/// so existing UI consumers that haven't migrated still get the
/// full backlog.
pub async fn get_harness_events(
    State(app): State<ChatAppState>,
    Path(session_id): Path<Uuid>,
    axum::extract::Query(query): axum::extract::Query<std::collections::HashMap<String, String>>,
    Extension(principal): Extension<RequestPrincipal>,
) -> impl IntoResponse {
    tracing::debug!(
        session_id = %session_id,
        principal = ?principal,
        "get_harness_events: principal extracted"
    );
    let Some(session) = app.conversation.get_session(session_id).await else {
        return (StatusCode::NOT_FOUND, "session not found").into_response();
    };
    let events: Vec<serde_json::Value> = session
        .harness_events
        .iter()
        .map(|ev| {
            serde_json::json!({
                "kind": ev.kind,
                "taskId": ev.task_id,
                "status": ev.status,
                "detail": ev.detail,
                "remote": ev.remote,
                "timestamp": ev.timestamp,
            })
        })
        .collect();
    let params = super::PaginationParams::from_query(&query);
    let page = super::PaginatedPage::from_slice(&events, params);
    Json(serde_json::json!({
        "session_id": session_id,
        // Legacy field — kept for any UI client still reading
        // `events` from this endpoint. Carries the same page slice
        // as `data` so the two shapes stay consistent.
        "events": page.data,
        // Paginated envelope.
        "data": page.data,
        "next_cursor": page.next_cursor,
        "has_more": page.has_more,
    }))
    .into_response()
}

/// Read the session's decision audit trail, either from the emitted
/// package's `runtime/decisions.jsonl` (post-emission) or from the
/// in-memory `session.decisions` Vec (pre-emission). `?filter=<kind>`
/// narrows to one DecisionType variant (snake_case, matching the
/// internally-tagged serde rename).
///
/// Supports `?cursor=<opaque>&limit=<n>` (default
/// 100, max 1000). The legacy unpaginated `decisions` array is
/// preserved alongside the new paginated envelope so existing UI
/// consumers keep working unchanged.
pub async fn get_decisions(
    State(app): State<ChatAppState>,
    Path(session_id): Path<Uuid>,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
    Extension(principal): Extension<RequestPrincipal>,
) -> impl IntoResponse {
    tracing::debug!(
        session_id = %session_id,
        principal = ?principal,
        "get_decisions: principal extracted"
    );
    let Some(session) = app.conversation.get_session(session_id).await else {
        return (StatusCode::NOT_FOUND, "session not found").into_response();
    };
    let filter = params.get("filter").cloned();

    // Merge on-disk + in-memory decisions, deduplicating by
    // (timestamp, decision.kind). On-disk is the canonical RO-Crate
    // audit artifact written at emit time and persisted-on-write for
    // post-emit decisions; in-memory is the live source for any
    // record that hasn't yet been flushed (race) or that the agent
    // recorded but the server hasn't observed yet. Reading both and
    // dedup'ing means the Decisions tab never silently misses a
    // record — previously the disk-then-fallback path returned only
    // the initial confirm record because the file is frozen at emit.
    let mut records: Vec<serde_json::Value> = Vec::new();
    let mut seen_keys: std::collections::HashSet<(String, String)> =
        std::collections::HashSet::new();
    let key_of = |v: &serde_json::Value| -> (String, String) {
        let ts = v
            .get("timestamp")
            .and_then(|t| t.as_str())
            .unwrap_or("")
            .to_string();
        let kind = v
            .get("decision")
            .and_then(|d| d.get("kind"))
            .and_then(|k| k.as_str())
            .unwrap_or("")
            .to_string();
        (ts, kind)
    };
    // Predicate: a record matches the typed DecisionRecord shape only
    // when `.decision.kind` is a non-empty string. The agent (claude
    // subprocess) sometimes appends free-form audit entries to
    // runtime/decisions.jsonl with `kind` at the TOP level (not nested
    // under `decision.kind`) — e.g.,
    // {"ts":"...","kind":"discovery_blocker","task_id":"...",...}
    // {"decision_type":"rerun_after_upstream_amendment",...}
    // These violate the typed taxonomy in DecisionType.ts and crash
    // any consumer that walks `record.decision.kind`. We drop them
    // silently here so the API surface stays clean. The agent is told
    // (via PROMPT.md) to write its own audit to runtime/LOG.jsonl;
    // misplaced lines stay in the on-disk file (we don't rewrite the
    // RO-Crate artifact) but never reach the wire.
    let is_typed_decision = |v: &serde_json::Value| -> bool {
        v.get("decision")
            .and_then(|d| d.get("kind"))
            .and_then(|k| k.as_str())
            .map(|s| !s.is_empty())
            .unwrap_or(false)
    };
    if let Some(pkg) = &session.emitted_package_path {
        let path = pkg.join("runtime").join("decisions.jsonl");
        if let Ok(raw) = tokio::fs::read_to_string(&path).await {
            for line in raw.lines() {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(line) {
                    if !is_typed_decision(&v) {
                        continue;
                    }
                    let k = key_of(&v);
                    if seen_keys.insert(k) {
                        records.push(v);
                    }
                }
            }
        }
    }
    for d in &session.decisions {
        if let Ok(v) = serde_json::to_value(d) {
            // Belt-and-braces: the in-memory path is already typed
            // DecisionRecord so this branch should always pass, but
            // the predicate guards against any future shape drift.
            if !is_typed_decision(&v) {
                continue;
            }
            let k = key_of(&v);
            if seen_keys.insert(k) {
                records.push(v);
            }
        }
    }
    // Sort by timestamp ascending so the UI's "newest first" rendering
    // works off a stable order regardless of which side discovered
    // each record.
    records.sort_by(|a, b| {
        let ta = a.get("timestamp").and_then(|t| t.as_str()).unwrap_or("");
        let tb = b.get("timestamp").and_then(|t| t.as_str()).unwrap_or("");
        ta.cmp(tb)
    });

    if let Some(kind) = filter {
        records.retain(|r| {
            r.get("decision")
                .and_then(|d| d.get("kind"))
                .and_then(|v| v.as_str())
                == Some(kind.as_str())
        });
    }

    let page_params = super::PaginationParams::from_query(&params);
    let page = super::PaginatedPage::from_slice(&records, page_params);

    Json(serde_json::json!({
        "session_id": session_id,
        // Legacy field — kept for any UI client still reading
        // `decisions` from this endpoint.
        "decisions": page.data,
        // Paginated envelope.
        "data": page.data,
        "next_cursor": page.next_cursor,
        "has_more": page.has_more,
    }))
    .into_response()
}

/// `POST /api/chat/session` — create a new chat session and return its greeting turn.
#[tracing::instrument(skip(app, headers, req), fields(session_id = tracing::field::Empty))]
pub async fn create_session(
    State(app): State<ChatAppState>,
    headers: axum::http::HeaderMap,
    BoundedJson(req): BoundedJson<CreateSessionRequest>,
) -> impl IntoResponse {
    // Resolve the requesting user (multi-user posture). The server has
    // no built-in auth — it trusts a fronting reverse proxy to set
    // either header below after performing real authentication. When
    // neither header is present (single-user development, or the proxy
    // hasn't enforced auth yet), the session falls back to whatever
    // `Session::new` derived from the server-process `$USER` env.
    //
    // Do NOT trust X-Scripps-User from the client directly in
    // production — it should be stripped at the proxy edge and
    // re-injected from authenticated identity.
    let owner_user = owner_user_from_headers(&headers);

    match app.conversation.start_session(req.careful_mode).await {
        Ok((id, greeting)) => {
            tracing::Span::current().record("session_id", id.to_string().as_str());
            // If the request carried a user header, override the
            // env-derived owner_user that Session::new put in place.
            // Best-effort: if the override fails, log and continue —
            // the session is still usable, just with a less specific
            // owner attribution.
            apply_owner_user(&app, id, owner_user, "create_session").await;
            Json(CreateSessionResponse {
                session_id: id,
                greeting,
            })
            .into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

/// `POST /api/chat/session/from-intent` — create a session pre-seeded with structured intent.
#[tracing::instrument(skip(app, headers, req), fields(session_id = tracing::field::Empty))]
pub async fn create_session_from_intent(
    State(app): State<ChatAppState>,
    headers: axum::http::HeaderMap,
    BoundedJson(req): BoundedJson<StartSessionFromIntentRequest>,
) -> impl IntoResponse {
    let goal = req.goal.trim();
    let modality = req.modality.trim();
    if goal.is_empty() {
        return (StatusCode::BAD_REQUEST, "goal is required").into_response();
    }
    if modality.is_empty() {
        return (StatusCode::BAD_REQUEST, "modality is required").into_response();
    }

    let prose = structured_intent_prose(&req);
    let owner_user = owner_user_from_headers(&headers);
    match app
        .conversation
        .start_session_from_prose(req.careful_mode, prose)
        .await
    {
        Ok((id, greeting)) => {
            tracing::Span::current().record("session_id", id.to_string().as_str());
            apply_owner_user(&app, id, owner_user, "create_session_from_intent").await;
            Json(CreateSessionResponse {
                session_id: id,
                greeting,
            })
            .into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

fn owner_user_from_headers(headers: &axum::http::HeaderMap) -> Option<String> {
    headers
        .get("X-Scripps-User")
        .or_else(|| headers.get("X-Forwarded-User"))
        .and_then(|v| v.to_str().ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

async fn apply_owner_user(app: &ChatAppState, id: Uuid, owner_user: Option<String>, label: &str) {
    let Some(user) = owner_user else {
        return;
    };
    let store = app.conversation.store_handle();
    if let Err(e) = store
        .update(id, move |s| {
            s.owner_user = user.clone();
            Ok(())
        })
        .await
    {
        // Structured tracing so the
        // session id stays in the TraceLayer pipeline instead of leaking
        // to raw stderr / journald.
        tracing::debug!(
            label = %label,
            session_id = ?id,
            error = %e,
            "owner_user override failed",
        );
    }
}

fn structured_intent_prose(req: &StartSessionFromIntentRequest) -> String {
    let modality = req.modality.trim();
    let mut lines = vec![
        format!("Goal: {}", req.goal.trim()),
        format!("Modality: {} ({})", modality, modality_label(modality)),
    ];
    let organism = req.organism.trim();
    if !organism.is_empty() {
        lines.push(format!("Organism: {organism}"));
    }
    let desired_outputs = req.desired_outputs.trim();
    if !desired_outputs.is_empty() {
        lines.push(format!("Desired outputs: {desired_outputs}"));
    }
    let uncertainties = req.uncertainties.trim();
    if !uncertainties.is_empty() {
        lines.push(format!("Uncertainties: {uncertainties}"));
    }
    lines.join("\n")
}

fn modality_label(modality: &str) -> &'static str {
    match modality {
        "bulk_rnaseq" => "bulk RNA-seq",
        "single_cell_rnaseq" => "single-cell RNA-seq",
        "variant_calling" => "variant calling",
        "chip_seq" => "ChIP-seq",
        "metagenomics" => "metagenomics",
        "proteomics" => "proteomics",
        "generic_omics" => "other omics",
        _ => "unspecified modality",
    }
}

/// `GET /api/chat/session/:id/state` — return a `SessionStateSnapshot` for the given session.
#[tracing::instrument(skip(app), fields(session_id = %session_id))]
pub async fn get_state(
    State(app): State<ChatAppState>,
    Path(session_id): Path<Uuid>,
) -> impl IntoResponse {
    match app.conversation.get_session(session_id).await {
        Some(session) => {
            // Short-circuit the sibling-package disk scan when the
            // cached entry for this session is still valid. Cache is
            // invalidated by `post_set_task_state` (the authoritative
            // state-write surface) so a stale read is impossible
            // without a matching invalidation between the write and
            // the next GET. Cache miss / stale falls through to the
            // reconciliation walk + repopulate.
            let cached = match app.reconciled_progress_cache.get(&session_id) {
                Some(entry) if entry.valid => {
                    Some((entry.progress.clone(), entry.blocked_tasks.clone()))
                }
                _ => None,
            };
            // Always go through `reconciled_progress_and_blocked` for cache
            // misses: the harness may have written to WORKFLOW.json after the
            // in-memory DAG snapshot was taken (long-running execution path,
            // see `get_state_reconciles_blocked_tasks_from_workflow_json`).
            // The earlier "single-emission fast-path" hypothesis was wrong —
            // even single-emission sessions can have on-disk state ahead of
            // memory. The reconcile fn handles single + multi-emission
            // candidates uniformly.
            let (progress, blocked_tasks) = match cached {
                Some(payload) => payload,
                None => {
                    let recomputed = reconciled_progress_and_blocked(&session).await;
                    // Repopulate the cache for the next reader.
                    if let Some((p, bt)) = recomputed.as_ref() {
                        app.reconciled_progress_cache.insert(
                            session_id,
                            super::app_state::ReconciledProgressEntry {
                                progress: p.clone(),
                                blocked_tasks: bt.clone(),
                                valid: true,
                            },
                        );
                    }
                    recomputed.unwrap_or_else(|| {
                        let dag = session.current_dag();
                        let progress = dag
                            .as_ref()
                            .map(|d| {
                                let (c, r, b, p) = d.progress();
                                ProgressSummary {
                                    completed: c,
                                    ready: r,
                                    blocked: b,
                                    pending: p,
                                }
                            })
                            .unwrap_or_default();
                        let blocked_tasks = dag.as_ref().map(dag_blocked_tasks).unwrap_or_default();
                        (progress, blocked_tasks)
                    })
                }
            };
            let snapshot = SessionStateSnapshot {
                session_id: session.id,
                state: session.state.clone(),
                // The `user_confirmed` wire field stays for
                // SessionStateSnapshot consumers, but its value comes
                // from `is_confirmed()` (token + pending_emission_id
                // + summary_hash drift check) rather than the legacy
                // bool field.
                user_confirmed: session.is_confirmed(),
                last_activity: session.last_activity,
                task_count: session.current_dag().map(|d| d.tasks.len()).unwrap_or(0),
                progress,
                emitted_package_path: session.emitted_package_path.clone(),
                title: session.title.clone(),
                parent_session_id: session
                    .lineage
                    .as_ref()
                    .map(|l| l.parent_session_id.to_string()),
                blocked_tasks,
                pending_input_hints: session.pending_input_hints.clone(),
            };
            // R3.8: surface the session's ETag so the UI can echo it
            // back as `If-Match` on subsequent confirm/reject/unblock/
            // branch mutations. Optimistic-concurrency only — absence
            // is tolerated server-side for back-compat.
            let mut response = Json(snapshot).into_response();
            super::insert_etag(response.headers_mut(), &session);
            response
        }
        None => (StatusCode::NOT_FOUND, "session not found").into_response(),
    }
}

/// Recompute progress counts from the on-disk WORKFLOW.json when the
/// session has an emitted package. Returns None when there's no
/// package or the file can't be parsed — caller falls back to the
/// in-memory DAG.
///
/// Double-emit handling: when `emit_package` fires twice (observed
/// on the IVD live spec path), the session's emitted_package_path
/// points to the LAST emission but the running harness is bound to
/// the FIRST emission. Each directory gets its own WORKFLOW.json,
/// which the harness mutates independently. Pick whichever file
/// shows more non-pending tasks — that's the one the agent is
/// actually advancing.
async fn reconciled_progress_and_blocked(
    session: &scripps_workflow_conversation::Session,
) -> Option<(ProgressSummary, Vec<String>)> {
    let state_path = session.emitted_package_path.as_ref()?.clone();
    tokio::task::spawn_blocking(move || reconciled_progress_and_blocked_sync(state_path))
        .await
        .ok()
        .flatten()
}

fn dag_blocked_tasks(dag: &scripps_workflow_core::dag::DAG) -> Vec<String> {
    use scripps_workflow_core::dag::TaskState;
    dag.tasks
        .iter()
        .filter(|(_, t)| matches!(t.state, TaskState::Blocked { .. }))
        .map(|(id, _)| id.to_string())
        .collect()
}

fn reconciled_progress_and_blocked_sync(
    state_path: std::path::PathBuf,
) -> Option<(ProgressSummary, Vec<String>)> {
    let mut candidate_paths: Vec<std::path::PathBuf> = vec![state_path.clone()];
    // Sibling paths that look like `<uuid>-<modality>-<ts>` with
    // the same prefix — any of them could be the harness's actual
    // package. Cheap disk scan; the packages dir is flat.
    if let Some(parent) = state_path.parent() {
        if let Ok(entries) = std::fs::read_dir(parent) {
            let prefix = state_path
                .file_name()
                .and_then(|n| n.to_str())
                .and_then(|s| s.split('-').take(6).collect::<Vec<_>>().join("-").into())
                .unwrap_or_default();
            for entry in entries.flatten() {
                let p = entry.path();
                if p == state_path {
                    continue;
                }
                let name = p.file_name().and_then(|n| n.to_str()).unwrap_or_default();
                if !prefix.is_empty() && name.starts_with(&prefix) {
                    candidate_paths.push(p);
                }
            }
        }
    }
    let mut best: Option<(usize, ProgressSummary, Vec<String>)> = None;
    for pkg in &candidate_paths {
        let Ok(content) = std::fs::read_to_string(pkg.join("WORKFLOW.json")) else {
            continue;
        };
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&content) else {
            continue;
        };
        let Some(tasks) = value.get("tasks").and_then(|t| t.as_object()) else {
            continue;
        };
        let mut completed = 0usize;
        let mut ready = 0usize;
        let mut blocked = 0usize;
        let mut pending = 0usize;
        let mut blocked_tasks = Vec::new();
        for (id, t) in tasks.iter() {
            let status = t
                .get("state")
                .and_then(|s| s.get("status"))
                .and_then(|v| v.as_str())
                .unwrap_or("pending");
            match status {
                "completed" => completed += 1,
                "ready" | "running" => ready += 1,
                "blocked" => {
                    blocked += 1;
                    blocked_tasks.push(id.clone());
                }
                _ => pending += 1,
            }
        }
        let non_pending = completed + ready + blocked;
        let summary = ProgressSummary {
            completed,
            ready,
            blocked,
            pending,
        };
        // Bind the prior candidate in the arm pattern
        // rather than re-unwrapping `best` after the match. The arm
        // guard already proves `best.is_some()`, but the inline
        // `best.unwrap()` was a redundant panic surface in a request-
        // path code site; the rewrite is panic-free by construction.
        best = Some(match best {
            Some((prev, prev_summary, prev_blocked_tasks)) if prev >= non_pending => {
                (prev, prev_summary, prev_blocked_tasks)
            }
            _ => (non_pending, summary, blocked_tasks),
        });
    }
    best.map(|(_, s, blocked_tasks)| (s, blocked_tasks))
}

/// When `?cursor=` or `?limit=` is present, this
/// endpoint returns a `Page<Turn>` envelope (`{ data, next_cursor,
/// has_more }`). When neither query parameter is present the legacy
/// shape (a bare `Vec<Turn>`) is returned so existing UI consumers and
/// fixtures keep working unchanged. The bifurcation is intentional —
/// the chat UI's `useConversation` hook walks the transcript as a
/// single array and pagination would force a UI rewrite that isn't on
/// the critical path; new clients (long-running ops dashboards, the
/// session-history exporter) opt in by passing pagination params.
#[tracing::instrument(skip(app, query), fields(session_id = %session_id))]
pub async fn get_transcript(
    State(app): State<ChatAppState>,
    Path(session_id): Path<Uuid>,
    axum::extract::Query(query): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let Some(session) = app.conversation.get_session(session_id).await else {
        return (StatusCode::NOT_FOUND, "session not found").into_response();
    };
    // Legacy unpaginated shape: no query params at all.
    let wants_pagination = query.contains_key("cursor") || query.contains_key("limit");
    if !wants_pagination {
        return Json(session.conversation.clone()).into_response();
    }
    let params = super::PaginationParams::from_query(&query);
    let page = super::PaginatedPage::from_slice(&session.conversation, params);
    Json(page).into_response()
}

/// `GET /api/chat/session/:id/metrics` — return the per-session telemetry snapshot.
#[tracing::instrument(skip(app), fields(session_id = %session_id))]
pub async fn get_metrics(
    State(app): State<ChatAppState>,
    Path(session_id): Path<Uuid>,
) -> impl IntoResponse {
    if app.conversation.get_session(session_id).await.is_none() {
        return (StatusCode::NOT_FOUND, "session not found").into_response();
    }
    match app.conversation.metrics_snapshot(session_id).await {
        Some(snapshot) => Json(snapshot).into_response(),
        // Empty-metrics is a valid pollable state for a fresh session
        // (Performance tab polls /metrics every 15s); return 200 + null
        // so the UI doesn't spam "Failed to load resource".
        None => Json(serde_json::Value::Null).into_response(),
    }
}

/// Run the rubric scorer against the session's current transcript.
/// Bills through `MetricsStore::record_scorer_usage` under the session
/// id, so the resulting spend appears in the Performance tab's
/// `scorer_cost_usd` row. Returns 404 when the session is unknown and
/// 400 when no turns have been recorded yet (an empty session has
/// nothing to score).
///
/// §R8: cached for 30 s keyed by
/// (session_id, transcript length). Re-clicks within the window OR
/// against an unchanged transcript return the cached score without an
/// LLM call. Each scorer call costs ~$0.025; the cache stops UI
/// re-clicks from inflating `scorer_cost_usd` when the operator just
/// wants to re-check the same number.
pub async fn score_session(
    State(app): State<ChatAppState>,
    Path(session_id): Path<Uuid>,
) -> impl IntoResponse {
    // The scorer
    // is a Sonnet call that bills ~$0.025/invocation. Cap at 6/min so a
    // refresh-spammer can't run up the bill before the §R8 transcript-
    // hash cache amortizes the cost.
    if let Err(status) = LlmRateBuckets::check(
        &app.llm_buckets.score,
        session_id,
        app.llm_rate_limits.score,
    ) {
        return (
            status,
            "rate limit exceeded: /score capped at 6/min/session",
        )
            .into_response();
    }

    let Some(session) = app.conversation.get_session(session_id).await else {
        return (StatusCode::NOT_FOUND, "session not found").into_response();
    };
    let has_turns = app
        .conversation
        .metrics_snapshot(session_id)
        .await
        .map(|m| m.turn_count > 0)
        .unwrap_or(false);
    if !has_turns {
        return (StatusCode::BAD_REQUEST, "session has no transcript yet").into_response();
    }
    let transcript_len = session.conversation.len();

    // §R8 — cache hit short-circuit.
    {
        let cache = app.scorer_cache.read().await;
        if let Some((cached_len, cached_at, cached_score)) = cache.get(&session_id) {
            let fresh = cached_at.elapsed().as_secs() < SCORER_CACHE_TTL_SECS;
            let same_transcript = *cached_len == transcript_len;
            if fresh && same_transcript {
                return Json(*cached_score).into_response();
            }
        }
    }

    let backend = app.conversation.llm_for_scoring();
    match scripps_workflow_conversation::score_transcript(
        backend,
        app.conversation.metrics(),
        session_id,
        &session.conversation,
        "",
    )
    .await
    {
        Ok(score) => {
            // §R8 — populate the cache with the fresh score.
            let mut cache = app.scorer_cache.write().await;
            cache.insert(
                session_id,
                (transcript_len, std::time::Instant::now(), score),
            );
            Json(score).into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

/// Route inventory for the doc-as-contract gate +
/// per-submodule `routes()` builder. `mod.rs::router()` merges every
/// submodule's builder into the single chat surface.
pub(super) const ROUTES: &[(&str, &str)] = &[
    ("POST", "/api/chat/session"),
    ("POST", "/api/chat/session/from-intent"),
    ("GET", "/api/chat/session/:id/state"),
    ("GET", "/api/chat/session/:id/transcript"),
    ("GET", "/api/chat/session/:id/metrics"),
    ("POST", "/api/chat/session/:id/score"),
    ("GET", "/api/chat/session/:id/decisions"),
    ("GET", "/api/chat/session/:id/harness-events"),
];

pub(super) fn routes() -> axum::Router<ChatAppState> {
    axum::Router::new()
        .route("/api/chat/session", axum::routing::post(create_session))
        .route(
            "/api/chat/session/from-intent",
            axum::routing::post(create_session_from_intent),
        )
        .route("/api/chat/session/:id/state", axum::routing::get(get_state))
        .route(
            "/api/chat/session/:id/transcript",
            axum::routing::get(get_transcript),
        )
        .route(
            "/api/chat/session/:id/metrics",
            axum::routing::get(get_metrics),
        )
        .route(
            "/api/chat/session/:id/score",
            axum::routing::post(score_session),
        )
        .route(
            "/api/chat/session/:id/decisions",
            axum::routing::get(get_decisions),
        )
        .route(
            "/api/chat/session/:id/harness-events",
            axum::routing::get(get_harness_events),
        )
}

#[cfg(test)]
mod tests {
    use crate::chat_routes::test_support::{assistant, body_json, make_router, tool_use};
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use scripps_workflow_conversation::{BatchableTool, Tool};
    use tower::util::ServiceExt;

    // ── harness-events endpoint ───────────────────────────────────────────

    #[tokio::test]
    async fn harness_events_404_when_session_missing() {
        let (router, _) = make_router(vec![]).await;
        let bogus = uuid::Uuid::new_v4();
        let req = Request::builder()
            .method("GET")
            .uri(format!("/api/chat/session/{}/harness-events", bogus))
            .body(Body::empty())
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn harness_events_empty_on_fresh_session() {
        let (router, _) = make_router(vec![]).await;
        let req = Request::builder()
            .method("POST")
            .uri("/api/chat/session")
            .header("content-type", "application/json")
            .body(Body::from(r#"{}"#))
            .unwrap();
        let resp = router.clone().oneshot(req).await.unwrap();
        let body = body_json(resp.into_body()).await;
        let id = body["session_id"].as_str().unwrap().to_string();

        let req = Request::builder()
            .method("GET")
            .uri(format!("/api/chat/session/{}/harness-events", id))
            .body(Body::empty())
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp.into_body()).await;
        assert_eq!(body["session_id"], id);
        assert!(body["events"].as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn harness_events_returns_all_persisted_events() {
        use crate::chat_routes::test_support::seed_session_with_completed_task;
        let (router, app) = make_router(vec![]).await;
        let id = seed_session_with_completed_task(&app, "t_demo", None).await;
        // Inject a couple of harness events directly so the backlog is
        // non-empty. In production these are written by the server's
        // /progress handler.
        app.conversation
            .store_handle()
            .update(id, |s| {
                s.harness_events
                    .push(scripps_workflow_conversation::HarnessEvent {
                        kind: "task_started".into(),
                        task_id: "t_demo".into(),
                        status: "running".into(),
                        detail: "test".into(),
                        remote: None,
                        timestamp: chrono::Utc::now(),
                    });
                s.harness_events
                    .push(scripps_workflow_conversation::HarnessEvent {
                        kind: "task_completed".into(),
                        task_id: "t_demo".into(),
                        status: "completed".into(),
                        detail: "done".into(),
                        remote: None,
                        timestamp: chrono::Utc::now(),
                    });
                Ok(())
            })
            .await
            .unwrap();

        let req = Request::builder()
            .method("GET")
            .uri(format!("/api/chat/session/{}/harness-events", id))
            .body(Body::empty())
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        let body = body_json(resp.into_body()).await;
        let arr = body["events"].as_array().unwrap();
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0]["kind"], "task_started");
        assert_eq!(arr[0]["taskId"], "t_demo");
        assert_eq!(arr[1]["kind"], "task_completed");
    }

    // ── decisions endpoint ────────────────────────────────────────────────

    #[tokio::test]
    async fn decisions_404_when_session_missing() {
        let (router, _) = make_router(vec![]).await;
        let bogus = uuid::Uuid::new_v4();
        let req = Request::builder()
            .method("GET")
            .uri(format!("/api/chat/session/{}/decisions", bogus))
            .body(Body::empty())
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn decisions_returns_in_memory_when_no_package() {
        // Fresh session has no emitted package — endpoint falls back to
        // session.decisions Vec. A brand-new session carries zero
        // decisions, so the response is an empty array (not an error).
        let (router, _) = make_router(vec![]).await;
        let req = Request::builder()
            .method("POST")
            .uri("/api/chat/session")
            .header("content-type", "application/json")
            .body(Body::from(r#"{}"#))
            .unwrap();
        let resp = router.clone().oneshot(req).await.unwrap();
        let body = body_json(resp.into_body()).await;
        let id = body["session_id"].as_str().unwrap().to_string();

        let req = Request::builder()
            .method("GET")
            .uri(format!("/api/chat/session/{}/decisions", id))
            .body(Body::empty())
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp.into_body()).await;
        assert_eq!(body["session_id"], id);
        assert!(body["decisions"].as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn decisions_reads_jsonl_from_emitted_package_when_present() {
        use crate::chat_routes::test_support::seed_session_with_completed_task;
        let tmp = tempfile::TempDir::new().unwrap();
        let runtime = tmp.path().join("runtime");
        std::fs::create_dir_all(&runtime).unwrap();
        let jsonl = r#"{"timestamp":"2026-04-22T10:00:00Z","session_id":"x","decision":{"kind":"confirm"},"actor":"sme"}
{"timestamp":"2026-04-22T11:00:00Z","session_id":"x","decision":{"kind":"amend_stage","stage":"data_acquisition","method_prose":"multi_repo_processed_matrices_r"},"actor":"sme"}
"#;
        std::fs::write(runtime.join("decisions.jsonl"), jsonl).unwrap();

        let (router, app) = make_router(vec![]).await;
        let id =
            seed_session_with_completed_task(&app, "t_demo", Some(tmp.path().to_path_buf())).await;

        // No filter → both.
        let req = Request::builder()
            .method("GET")
            .uri(format!("/api/chat/session/{}/decisions", id))
            .body(Body::empty())
            .unwrap();
        let resp = router.clone().oneshot(req).await.unwrap();
        let body = body_json(resp.into_body()).await;
        assert_eq!(body["decisions"].as_array().unwrap().len(), 2);

        // filter=amend_stage → just the amendment.
        let req = Request::builder()
            .method("GET")
            .uri(format!(
                "/api/chat/session/{}/decisions?filter=amend_stage",
                id
            ))
            .body(Body::empty())
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        let body = body_json(resp.into_body()).await;
        let arr = body["decisions"].as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["decision"]["kind"], "amend_stage");
    }

    #[tokio::test]
    async fn create_session_returns_id_and_greeting() {
        let (router, _) = make_router(vec![]).await;
        let req = Request::builder()
            .method("POST")
            .uri("/api/chat/session")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"careful_mode": false}"#))
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp.into_body()).await;
        assert!(body["session_id"].is_string());
        assert!(body["greeting"]["content"]
            .as_str()
            .unwrap()
            .contains("Tell me about"));
    }

    #[tokio::test]
    async fn create_session_from_intent_records_intake_without_llm_turn() {
        let (router, app) = make_router(vec![]).await;
        let req = Request::builder()
            .method("POST")
            .uri("/api/chat/session/from-intent")
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::json!({
                    "goal": "Identify genes differentially expressed between treated and control samples.",
                    "modality": "bulk_rnaseq",
                    "organism": "Homo sapiens",
                    "desired_outputs": "Differential-expression table and volcano plot",
                    "uncertainties": "Batch covariates are not finalized"
                })
                .to_string(),
            ))
            .unwrap();
        let resp = router.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp.into_body()).await;
        let session_id = uuid::Uuid::parse_str(body["session_id"].as_str().unwrap()).unwrap();

        let session = app.conversation.get_session(session_id).await.unwrap();
        assert!(session
            .intake_prose
            .contains("differentially expressed between treated and control"));
        assert_eq!(
            session.classification.as_ref().map(|c| c.modality.as_str()),
            Some("bulk_rnaseq")
        );
        assert!(session.conversation.iter().any(|turn| {
            turn.role == scripps_workflow_conversation::TurnRole::User
                && turn.content.contains("Desired outputs")
        }));

        let req = Request::builder()
            .method("GET")
            .uri(format!("/api/chat/session/{}/state", session_id))
            .body(Body::empty())
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let state = body_json(resp.into_body()).await;
        assert_ne!(state["state"]["kind"], "greeting");
    }

    #[tokio::test]
    #[ignore = "pinned to composer_version=1; v4 path's discover-companion synthesis depends on atom-level `method_choice` (not `attributes.candidate_tools`), so the current catalog doesn't trigger the IntakeFollowup transition reliably. Coverage moves to fixture_runner."]
    async fn get_state_reflects_post_intake_progress() {
        // Ignored: the v4 archetype catalog doesn't author explicit
        // `discover_*` atoms, and the `discover_companion_synthesis`
        // synthesis path only fires for atoms with `method_choice`
        // (not `attributes.candidate_tools`). So the `single_cell_de`
        // archetype's batch_correction atom doesn't surface a
        // discover_batch_correction task to trigger `IntakeFollowup`.
        // Re-enable once the atom catalog normalizes the method-
        // discovery hint surface.
        let (router, _app) = make_router(vec![
            tool_use(Tool::Batchable(BatchableTool::AppendIntakeProse {
                prose: "single cell scRNA-seq human samples".into(),
            })),
            assistant("ok"),
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
            .body(Body::from(r#"{"message":"go"}"#))
            .unwrap();
        let _ = router.clone().oneshot(req).await.unwrap();

        let req = Request::builder()
            .method("GET")
            .uri(format!("/api/chat/session/{}/state", session_id))
            .body(Body::empty())
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp.into_body()).await;
        assert_eq!(body["state"]["kind"], "intake_followup");
        assert!(body["task_count"].as_u64().unwrap() > 0);
    }

    #[tokio::test]
    async fn get_state_reconciles_blocked_tasks_from_workflow_json() {
        use crate::chat_routes::test_support::seed_session_with_completed_task;
        let pkg = tempfile::tempdir().unwrap();
        std::fs::write(
            pkg.path().join("WORKFLOW.json"),
            serde_json::json!({
                "tasks": {
                    "blocked_on_disk": {
                        "state": {
                            "status": "blocked",
                            "record": { "reason": "needs input", "attempts": [] }
                        }
                    },
                    "done_on_disk": {
                        "state": {
                            "status": "completed",
                            "result": {}
                        }
                    }
                }
            })
            .to_string(),
        )
        .unwrap();

        let (router, app) = make_router(vec![]).await;
        let id =
            seed_session_with_completed_task(&app, "stale_memory", Some(pkg.path().to_path_buf()))
                .await;

        let req = Request::builder()
            .method("GET")
            .uri(format!("/api/chat/session/{}/state", id))
            .body(Body::empty())
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp.into_body()).await;
        assert_eq!(body["progress"]["blocked"].as_u64(), Some(1));
        assert_eq!(
            body["blocked_tasks"].as_array().unwrap(),
            &vec![serde_json::json!("blocked_on_disk")]
        );
    }

    #[tokio::test]
    async fn unknown_session_returns_404() {
        let (router, _) = make_router(vec![]).await;
        let req = Request::builder()
            .method("GET")
            .uri(format!("/api/chat/session/{}/state", uuid::Uuid::new_v4()))
            .body(Body::empty())
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn metrics_endpoint_returns_recorded_turn() {
        let (router, _) = make_router(vec![assistant("hi there")]).await;

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
            .body(Body::from(r#"{"message":"hello"}"#))
            .unwrap();
        let _ = router.clone().oneshot(req).await.unwrap();

        let req = Request::builder()
            .method("GET")
            .uri(format!("/api/chat/session/{}/metrics", session_id))
            .body(Body::empty())
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp.into_body()).await;
        assert_eq!(body["turn_count"], 1);
        assert!(body["mean_turn_ms"].as_u64().is_some());
    }

    #[tokio::test]
    async fn metrics_endpoint_404_for_unknown_session() {
        let (router, _) = make_router(vec![]).await;
        let id = uuid::Uuid::new_v4();
        let req = Request::builder()
            .method("GET")
            .uri(format!("/api/chat/session/{}/metrics", id))
            .body(Body::empty())
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn score_endpoint_returns_rubric_and_bills_into_session() {
        // Mock backend feeds two responses: one for the user turn, one
        // for the scorer call. Scorer response emits a valid 18/18.
        let scorer_response = "NATURALNESS: 2\n\
            CONTINUITY: 2\n\
            ONE_QUESTION: 2\n\
            METHOD_NEUTRALITY: 2\n\
            CLAIM_BOUNDARY: 2\n\
            TOOL_EFFICIENCY: 2\n\
            CONFIRMATION: 2\n\
            RECOVERY: 2\n\
            HARDWARE_AWARENESS: 2\n\
            TOTAL: 18\n";
        let (router, _app) =
            make_router(vec![assistant("hi there"), assistant(scorer_response)]).await;

        // Create session + send one turn so the transcript has content.
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
            .body(Body::from(r#"{"message":"hello"}"#))
            .unwrap();
        let _ = router.clone().oneshot(req).await.unwrap();

        // POST /score.
        let req = Request::builder()
            .method("POST")
            .uri(format!("/api/chat/session/{}/score", session_id))
            .body(Body::empty())
            .unwrap();
        let resp = router.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp.into_body()).await;
        assert_eq!(body["naturalness"], 2);
        assert_eq!(body["hardware_awareness"], 2);

        // Metrics snapshot: scorer bucket should be populated (cost 0
        // because MockLlmBackend returns Usage::default()).
        let req = Request::builder()
            .method("GET")
            .uri(format!("/api/chat/session/{}/metrics", session_id))
            .body(Body::empty())
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        let body = body_json(resp.into_body()).await;
        assert!(
            body["per_model_scorer_cost_usd"]["sonnet_4_6"].is_number(),
            "scorer bucket should be initialized: {}",
            body
        );
    }

    #[tokio::test]
    async fn score_endpoint_caches_within_window() {
        // §R8 regression: re-clicking /score with the same transcript
        // within 30s must return the cached score WITHOUT a second LLM
        // call. We verify by feeding only ONE scorer response in the
        // mock backend: if the cache works, the second /score call
        // returns the cached value; if it doesn't, the second call
        // hits the backend, finds it exhausted, and 500s.
        let scorer_response = "NATURALNESS: 1\n\
            CONTINUITY: 1\n\
            ONE_QUESTION: 1\n\
            METHOD_NEUTRALITY: 2\n\
            CLAIM_BOUNDARY: 1\n\
            TOOL_EFFICIENCY: 2\n\
            CONFIRMATION: 1\n\
            RECOVERY: 1\n\
            HARDWARE_AWARENESS: 2\n\
            TOTAL: 12\n";
        // Only ONE scorer response — second /score call MUST hit cache.
        let (router, _app) = make_router(vec![assistant("hi"), assistant(scorer_response)]).await;
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
            .body(Body::from(r#"{"message":"hi"}"#))
            .unwrap();
        let _ = router.clone().oneshot(req).await.unwrap();
        // First /score — fresh LLM call.
        let req1 = Request::builder()
            .method("POST")
            .uri(format!("/api/chat/session/{}/score", session_id))
            .body(Body::empty())
            .unwrap();
        let resp1 = router.clone().oneshot(req1).await.unwrap();
        assert_eq!(resp1.status(), StatusCode::OK);
        let body1 = body_json(resp1.into_body()).await;
        assert_eq!(body1["naturalness"], 1);
        // Second /score — cache hit, no LLM call (mock would be exhausted).
        let req2 = Request::builder()
            .method("POST")
            .uri(format!("/api/chat/session/{}/score", session_id))
            .body(Body::empty())
            .unwrap();
        let resp2 = router.oneshot(req2).await.unwrap();
        assert_eq!(
            resp2.status(),
            StatusCode::OK,
            "second /score within cache window must hit cache, not exhaust the backend",
        );
        let body2 = body_json(resp2.into_body()).await;
        assert_eq!(body2["naturalness"], 1);
        assert_eq!(body1, body2, "cached score must equal fresh score");
    }

    #[tokio::test]
    async fn score_endpoint_400s_on_empty_transcript() {
        let (router, _) = make_router(vec![]).await;
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

        // No user turn sent — only the greeting exists, which isn't a
        // recorded chat turn. /score must refuse with 400.
        let req = Request::builder()
            .method("POST")
            .uri(format!("/api/chat/session/{}/score", session_id))
            .body(Body::empty())
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn score_endpoint_404s_on_unknown_session() {
        let (router, _) = make_router(vec![]).await;
        let id = uuid::Uuid::new_v4();
        let req = Request::builder()
            .method("POST")
            .uri(format!("/api/chat/session/{}/score", id))
            .body(Body::empty())
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    /// /score is
    /// capped at 6/min per session. Burst past the budget and the 7th
    /// request must 429.
    #[tokio::test]
    async fn score_endpoint_rate_limited_at_six_per_minute() {
        // No scorer responses needed — even if every request hits the
        // bucket guard early we won't get past the gate.
        let (router, _app) = make_router(vec![]).await;
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

        // Issue 10 /score POSTs in tight succession. The first 6 may
        // proceed (then fall out as 400 with "no transcript yet"); the
        // 7th+ must return 429 from the rate-limit guard before the
        // 400 even fires.
        let mut statuses: Vec<u16> = Vec::new();
        for _ in 0..10 {
            let req = Request::builder()
                .method("POST")
                .uri(format!("/api/chat/session/{}/score", session_id))
                .body(Body::empty())
                .unwrap();
            let resp = router.clone().oneshot(req).await.unwrap();
            statuses.push(resp.status().as_u16());
        }
        let too_many = statuses
            .iter()
            .filter(|s| **s == StatusCode::TOO_MANY_REQUESTS.as_u16())
            .count();
        assert!(
            too_many > 0,
            "expected at least one 429 in {} successive /score POSTs, got statuses {:?}",
            statuses.len(),
            statuses,
        );
    }

    // ── Multi-user attribution (Phase F) ────────────────────────────────

    #[tokio::test]
    async fn get_decisions_filters_malformed_agent_audit_entries() {
        // Regression: the agent (claude subprocess) appends free-form
        // audit entries to runtime/decisions.jsonl that don't match the
        // typed DecisionRecord schema — `kind` lives at the top level
        // instead of nested under `decision.kind`. Without this filter
        // the UI's record.decision.kind access crashes the State
        // Inspector + TaskDetailDrawer ("can't access property kind,
        // decision is undefined"). The /decisions endpoint MUST drop
        // those records silently while preserving the on-disk
        // RO-Crate artifact (we don't rewrite decisions.jsonl —
        // operators can audit the raw file separately).
        use crate::chat_routes::test_support::seed_session_with_completed_task;
        let pkg = tempfile::tempdir().unwrap();
        let runtime = pkg.path().join("runtime");
        std::fs::create_dir_all(&runtime).unwrap();

        // Mixed payload: 2 well-formed typed records, 3 malformed
        // agent free-form entries (3 different mis-shapes), 1 blank
        // line (whitespace-only).
        let jsonl = [
            // valid: confirm
            r#"{"timestamp":"2026-04-29T10:00:00Z","session_id":"00000000-0000-0000-0000-000000000000","decision":{"kind":"confirm"},"actor":"sme"}"#,
            // valid: branch
            r#"{"timestamp":"2026-04-29T10:01:00Z","session_id":"00000000-0000-0000-0000-000000000000","decision":{"kind":"branch","child_session_id":"11111111-1111-1111-1111-111111111111"},"actor":"sme"}"#,
            // INVALID: agent's discovery_blocker style — `kind` at top, no `decision` nesting
            r#"{"ts":"2026-04-29T10:02:00Z","kind":"discovery_blocker","task_id":"discover_data_acquisition","top_candidate":"hybrid"}"#,
            // INVALID: agent's rerun_after_upstream_amendment — uses `decision_type` field, no `decision.kind`
            r#"{"timestamp":"2026-04-29T10:03:00Z","decision_type":"rerun_after_upstream_amendment","task_id":"metadata_harmonization","method_id":"geo_soft_scrape"}"#,
            // INVALID: explicit decision: null
            r#"{"timestamp":"2026-04-29T10:04:00Z","session_id":"00000000-0000-0000-0000-000000000000","decision":null,"actor":"llm"}"#,
            "",
            // INVALID: decision.kind is empty string
            r#"{"timestamp":"2026-04-29T10:05:00Z","session_id":"00000000-0000-0000-0000-000000000000","decision":{"kind":""},"actor":"llm"}"#,
        ]
        .join("\n");
        std::fs::write(runtime.join("decisions.jsonl"), jsonl).unwrap();

        let (router, app) = make_router(vec![]).await;
        let id =
            seed_session_with_completed_task(&app, "t_demo", Some(pkg.path().to_path_buf())).await;

        let req = Request::builder()
            .method("GET")
            .uri(format!("/api/chat/session/{}/decisions", id))
            .body(Body::empty())
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp.into_body()).await;

        // Only the 2 typed records should reach the wire.
        let decisions = body["decisions"].as_array().unwrap();
        let kinds: Vec<&str> = decisions
            .iter()
            .filter_map(|d| d["decision"]["kind"].as_str())
            .collect();
        assert_eq!(
            kinds,
            vec!["confirm", "branch"],
            "only typed decisions must surface; got {:?}",
            kinds,
        );

        // None of the malformed shapes leaked through.
        for d in decisions {
            assert!(
                d.get("decision")
                    .and_then(|v| v.get("kind"))
                    .and_then(|v| v.as_str())
                    .map(|s| !s.is_empty())
                    .unwrap_or(false),
                "every emitted record must have a non-empty .decision.kind",
            );
            assert!(
                !d.as_object().unwrap().contains_key("decision_type"),
                "agent's `decision_type` field must not surface",
            );
        }

        drop(pkg);
    }

    #[tokio::test]
    async fn create_session_persists_owner_user_from_x_scripps_user_header() {
        let (router, app) = make_router(vec![]).await;
        let req = Request::builder()
            .method("POST")
            .uri("/api/chat/session")
            .header("content-type", "application/json")
            .header("X-Scripps-User", "alan")
            .body(Body::from(r#"{"careful_mode": false}"#))
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp.into_body()).await;
        let id = uuid::Uuid::parse_str(body["session_id"].as_str().unwrap()).unwrap();
        let session = app.conversation.get_session(id).await.unwrap();
        assert_eq!(
            session.owner_user, "alan",
            "X-Scripps-User must be persisted as Session.owner_user"
        );
    }

    #[tokio::test]
    async fn create_session_falls_back_to_x_forwarded_user_header() {
        let (router, app) = make_router(vec![]).await;
        let req = Request::builder()
            .method("POST")
            .uri("/api/chat/session")
            .header("content-type", "application/json")
            .header("X-Forwarded-User", "morgan")
            .body(Body::from(r#"{"careful_mode": false}"#))
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp.into_body()).await;
        let id = uuid::Uuid::parse_str(body["session_id"].as_str().unwrap()).unwrap();
        let session = app.conversation.get_session(id).await.unwrap();
        assert_eq!(
            session.owner_user, "morgan",
            "X-Forwarded-User must be respected when X-Scripps-User is absent"
        );
    }

    #[tokio::test]
    async fn x_scripps_user_takes_precedence_over_x_forwarded_user() {
        let (router, app) = make_router(vec![]).await;
        let req = Request::builder()
            .method("POST")
            .uri("/api/chat/session")
            .header("content-type", "application/json")
            .header("X-Scripps-User", "alan")
            .header("X-Forwarded-User", "morgan")
            .body(Body::from(r#"{"careful_mode": false}"#))
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        let body = body_json(resp.into_body()).await;
        let id = uuid::Uuid::parse_str(body["session_id"].as_str().unwrap()).unwrap();
        let session = app.conversation.get_session(id).await.unwrap();
        assert_eq!(
            session.owner_user, "alan",
            "X-Scripps-User must win when both headers present",
        );
    }

    #[tokio::test]
    async fn create_session_without_user_header_keeps_env_default() {
        let (router, app) = make_router(vec![]).await;
        let req = Request::builder()
            .method("POST")
            .uri("/api/chat/session")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"careful_mode": false}"#))
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        let body = body_json(resp.into_body()).await;
        let id = uuid::Uuid::parse_str(body["session_id"].as_str().unwrap()).unwrap();
        let session = app.conversation.get_session(id).await.unwrap();
        // Env default — either $USER (typical dev) or "local" (CI).
        // Just verify it's non-empty so the field is always meaningful.
        assert!(
            !session.owner_user.is_empty(),
            "owner_user must always have a non-empty fallback",
        );
    }

    /// E22 — `get_harness_events` reads `Extension<RequestPrincipal>`
    /// without panicking. The middleware stamps `Anonymous` when no
    /// auth credential is presented; the handler logs it and continues.
    /// A 200 response confirms the extension was available (a missing
    /// extension would panic with "missing request extension").
    #[tokio::test]
    async fn get_harness_events_reads_principal_extension() {
        let (router, _app) = make_router(vec![]).await;
        // Create a session first.
        let req = Request::builder()
            .method("POST")
            .uri("/api/chat/session")
            .header("content-type", "application/json")
            .body(Body::from(r#"{}"#))
            .unwrap();
        let resp = router.clone().oneshot(req).await.unwrap();
        let body = body_json(resp.into_body()).await;
        let id = body["session_id"].as_str().unwrap().to_string();

        // Now hit get_harness_events without any auth header.
        // The middleware stamps Anonymous; the handler must not panic.
        let req = Request::builder()
            .method("GET")
            .uri(format!("/api/chat/session/{}/harness-events", id))
            .body(Body::empty())
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        // 200 OK confirms the Extension<RequestPrincipal> was available.
        assert_eq!(
            resp.status(),
            StatusCode::OK,
            "get_harness_events must succeed with RequestPrincipal in scope"
        );
    }
}
