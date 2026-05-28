//! Per-task result surface + artifact fetch + active/stuck task
//! enumeration + freeform SME notes.
//!
//! Endpoints (plan §S16.2):
//! - GET /session/:id/task/:task_id/result
//! - GET /session/:id/artifacts/*path
//! - GET /session/:id/active-tasks
//! - GET /session/:id/stuck-tasks
//! - POST /session/:id/task/:task_id/note
//!
//! Owns the artifact-cache helper used by `get_task_result` and the
//! `read_running_tasks` WORKFLOW.json scanner used by the stuck-task
//! detector.

use super::super::*;
use super::{config_dir_or_default, mime_for_path};
use axum::{
    body::Body,
    extract::State,
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use tokio::io::AsyncReadExt;
use uuid::Uuid;

/// Cap on bytes read when slurping a JSON sidecar
/// (`runtime/cross-version-diff.json`, `WORKFLOW.json`) into the
/// per-task result endpoint. Unbounded `tokio::fs::read` /
/// `read_to_string` would allocate the whole file before parsing — a
/// multi-hundred-MB WORKFLOW.json (e.g. from an outsized expansion
/// after `Cardinality::IterateUntil` unrolling, or a runaway agent
/// rewriting the diff sidecar) would pin that much memory per GET
/// request on top of the 30 s polling cadence the stuck-task detector
/// runs. 16 MB comfortably covers the largest WORKFLOW.json + diff
/// shapes that occur in practice.
const RESULT_SIDECAR_READ_CAP_BYTES: u64 = 16 * 1024 * 1024;

/// Tail-byte cap on the `progress.log` slurp used by `get_active_tasks`
/// to surface "last non-empty line" + most-recent `[step N/M]` marker.
/// The harness's append-only contract bounds progress.log to ~1000
/// lines, but an agent emitting verbose stdout into the same file can
/// blow past that. 256 KB is plenty for newest-first scan of the last
/// few hundred log lines while keeping memory bounded.
const PROGRESS_LOG_TAIL_CAP_BYTES: u64 = 256 * 1024;

/// Scan `<package_root>/runtime/<task_id>/` for direct file children and
/// render them as ArtifactRefs. Missing directory is not an error — returns
/// an empty Vec. Symlinks are not followed; any IO failure short-circuits
/// to an empty list so the endpoint never 500s on a transient fs hiccup.
fn scan_artifacts(package_root: &std::path::Path, task_id: &str) -> Vec<ArtifactRef> {
    let runtime_base = package_root.join("runtime");
    let Ok(runtime_dir) = super::super::_path_jail::safe_segment_join(&runtime_base, task_id)
    else {
        return Vec::new();
    };
    if super::super::_path_jail::assert_under_root(package_root, &runtime_dir).is_err() {
        return Vec::new();
    }
    let Ok(read_dir) = std::fs::read_dir(&runtime_dir) else {
        return Vec::new();
    };
    let mut out: Vec<ArtifactRef> = Vec::new();
    for entry in read_dir.flatten() {
        let path = entry.path();
        let Ok(meta) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        if !meta.file_type().is_file() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|n| n.to_str()).map(String::from) else {
            continue;
        };
        let rel = format!("runtime/{}/{}", task_id, name);
        out.push(ArtifactRef {
            name,
            relative_path: rel,
            size_bytes: meta.len(),
            mime_type: mime_for_path(&path).to_string(),
        });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// cache TTL for artifact listings. 60 s safety upper bound on
/// top of the dir-mtime keyed cache; if mtime changes (rerun wrote new
/// files) the cache also misses regardless of TTL.
const ARTIFACT_CACHE_TTL_SECS: u64 = 60;

/// cached artifact scan. Keyed by (session_id, task_id). The
/// runtime directory's mtime is the versioning key; a rerun that
/// writes new files bumps it. Falls back to the uncached scan on any
/// I/O error.
async fn scan_artifacts_cached(
    app: &ChatAppState,
    session_id: Uuid,
    package_root: &std::path::Path,
    task_id: &str,
) -> Vec<ArtifactRef> {
    let runtime_base = package_root.join("runtime");
    let Ok(runtime_dir) = super::super::_path_jail::safe_segment_join(&runtime_base, task_id)
    else {
        return Vec::new();
    };
    if super::super::_path_jail::assert_under_root(package_root, &runtime_dir).is_err() {
        return Vec::new();
    }
    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let dir_mtime_secs: u64 = match std::fs::metadata(&runtime_dir).and_then(|m| m.modified()) {
        Ok(m) => m
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0),
        Err(_) => return Vec::new(),
    };

    let cache_key = (session_id, task_id.to_string());
    if let Some(entry) = app.artifact_cache.get(&cache_key) {
        let (cached_mtime, cached_at, cached) = entry.value();
        if *cached_mtime == dir_mtime_secs
            && now_secs.saturating_sub(*cached_at) < ARTIFACT_CACHE_TTL_SECS
        {
            return cached.clone();
        }
    }

    let fresh = scan_artifacts(package_root, task_id);
    app.artifact_cache
        .insert(cache_key, (dir_mtime_secs, now_secs, fresh.clone()));
    fresh
}

/// per-task result surface, enriched with an
/// artifact index when the session's package has been emitted.
#[tracing::instrument(skip(app), fields(session_id = %session_id, task_id = %task_id))]
pub async fn get_task_result(
    State(app): State<ChatAppState>,
    Path((session_id, task_id)): Path<(Uuid, String)>,
) -> impl IntoResponse {
    let Some(session) = app.conversation.get_session(session_id).await else {
        return (StatusCode::NOT_FOUND, "session not found").into_response();
    };
    let Some(dag) = session.current_dag() else {
        return (StatusCode::NOT_FOUND, "no DAG built yet").into_response();
    };
    let Some(task) = dag.tasks.get(task_id.as_str()) else {
        return (StatusCode::NOT_FOUND, "task not found").into_response();
    };

    use scripps_workflow_core::dag::TaskState;
    let (status_label, extra) = match &task.state {
        TaskState::Completed { result } => ("completed", serde_json::json!({ "result": result })),
        TaskState::Failed { reason } => ("failed", serde_json::json!({ "reason": reason })),
        TaskState::Blocked { record } => ("blocked", serde_json::json!({ "record": record })),
        TaskState::Pending => ("pending", serde_json::json!({})),
        TaskState::Ready => ("ready", serde_json::json!({})),
        TaskState::Running { .. } => ("running", serde_json::json!({})),
    };

    let artifacts: Vec<ArtifactRef> = match (&session.emitted_package_path, &task.state) {
        (Some(root), TaskState::Completed { .. }) => {
            scan_artifacts_cached(&app, session_id, root, &task_id).await
        }
        _ => Vec::new(),
    };

    // If the package has a narrative artifact + the interpretation
    // policy has a `verifiableEntities` block, run claim verification
    // and attach the report. This is the reactive (GET-only) path — no
    // state mutation. The companion POST endpoint
    // `/session/:id/task/:task_id/verify` is what flips the session to
    // Blocked on mismatch.
    //
    // Fast-path: a sibling agent writes
    // `runtime/verification-reports/<task_id>.json` at emit time. When
    // the sidecar is present we deserialize it directly; otherwise we
    // fall back to live verification on the blocking pool so the async
    // worker isn't tied up in regex + filesystem walks.
    let verification = match (&session.emitted_package_path, &task.state) {
        (Some(root), TaskState::Completed { .. }) => {
            let sidecar = root
                .join("runtime")
                .join("verification-reports")
                .join(format!("{}.json", task_id));
            if let Ok(bytes) = tokio::fs::read(&sidecar).await {
                serde_json::from_slice::<
                    scripps_workflow_core::claim_verifier::ClaimVerificationReport,
                >(&bytes)
                .ok()
            } else {
                let config_dir = config_dir_or_default();
                let project_class = session.project_class;
                let decisions = session.decisions.clone();
                let is_confirmatory = session.mode.is_confirmatory();
                let root_clone = root.clone();
                let task_id_clone = task_id.clone();
                tokio::task::spawn_blocking(move || {
                    crate::verification::verify_task_with_context(
                        &root_clone,
                        &task_id_clone,
                        &config_dir,
                        project_class,
                        &decisions,
                        is_confirmatory,
                    )
                    .map(|v| v.report)
                })
                .await
                .ok()
                .flatten()
            }
        }
        _ => None,
    };

    // include the cross-version diff when present so
    // `ResultReviewTurnCard` can render concordance counts inline.
    // Bounded read: cap the sidecar at `RESULT_SIDECAR_READ_CAP_BYTES`
    // so a runaway diff producer can't pin hundreds of MB per result GET.
    let cross_version_diff: Option<serde_json::Value> = match &session.emitted_package_path {
        Some(root) => {
            let p = root.join("runtime").join("cross-version-diff.json");
            match tokio::fs::File::open(&p).await {
                Ok(file) => {
                    let mut buf = Vec::new();
                    match file
                        .take(RESULT_SIDECAR_READ_CAP_BYTES)
                        .read_to_end(&mut buf)
                        .await
                    {
                        Ok(_) => serde_json::from_slice::<serde_json::Value>(&buf).ok(),
                        Err(_) => None,
                    }
                }
                Err(_) => None,
            }
        }
        None => None,
    };

    // Per-task agent-code sidecar. Written by agent-claude.sh to
    // runtime/outputs/<task_id>/agent-code.json after each execution.
    // Present only after the task has actually run; absent for tasks
    // that have never been dispatched. Bounded read to avoid pinning
    // memory on a pathologically large prompt.
    let agent_code: Option<scripps_workflow_core::agent_code::AgentCodeRecord> =
        match &session.emitted_package_path {
            Some(root) => {
                let outputs_dir =
                    super::super::_path_jail::runtime_outputs_for_task(root, &task_id).ok();
                if let Some(dir) = outputs_dir {
                    let p = dir.join("agent-code.json");
                    match tokio::fs::File::open(&p).await {
                        Ok(file) => {
                            let mut buf = Vec::new();
                            match file
                                .take(RESULT_SIDECAR_READ_CAP_BYTES)
                                .read_to_end(&mut buf)
                                .await
                            {
                                Ok(_) => serde_json::from_slice::<
                                    scripps_workflow_core::agent_code::AgentCodeRecord,
                                >(&buf)
                                .ok(),
                                Err(_) => None,
                            }
                        }
                        Err(_) => None,
                    }
                } else {
                    None
                }
            }
            None => None,
        };

    let mut body = serde_json::json!({
        "task_id": task_id,
        "status": status_label,
        "description": task.description,
        "kind": task.kind,
        "artifacts": artifacts,
        "verification": verification,
        "cross_version_diff": cross_version_diff,
        "agent_code": agent_code,
    });
    if let Some(obj) = body.as_object_mut() {
        if let Some(extra_obj) = extra.as_object() {
            for (k, v) in extra_obj {
                obj.insert(k.clone(), v.clone());
            }
        }
    }
    Json(body).into_response()
}

/// Pick `Content-Disposition` for an
/// artifact filename. Dangerous extensions — anything that the browser
/// would normally render as a same-origin document with script
/// privileges — get `attachment` so the browser saves rather than
/// renders. Everything else gets `inline` (the historical default) so
/// PNGs, TSVs, and PDFs preview in the drawer.
///
/// The extension comparison is case-insensitive (matches MIME guessing
/// in `mime_for_path`). Filenames without an extension are treated as
/// inline — they can't be parsed as HTML/SVG without an explicit
/// Content-Type the server doesn't set on them.
///
/// The companion `Content-Security-Policy: sandbox` header (added in
/// `get_artifact` itself, regardless of extension) is the defense in
/// depth: even if MIME guessing misfires and an attacker tricks the
/// browser into rendering an artifact as HTML, the sandbox CSP
/// neutralizes inline script + same-origin fetch from that document.
pub(super) fn disposition_for(filename: &str) -> String {
    let ext = std::path::Path::new(filename)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    let attach = matches!(
        ext.as_str(),
        "html" | "htm" | "svg" | "js" | "mjs" | "xml" | "xhtml" | "wasm"
    );
    let kind = if attach { "attachment" } else { "inline" };
    format!("{kind}; filename=\"{filename}\"")
}

/// static artifact fetch, scoped to the session's
/// emitted package root. Traversal attempts return 403.
pub async fn get_artifact(
    State(app): State<ChatAppState>,
    Path((session_id, rel_path)): Path<(Uuid, String)>,
) -> impl IntoResponse {
    let Some(session) = app.conversation.get_session(session_id).await else {
        return (StatusCode::NOT_FOUND, "session not found").into_response();
    };
    let Some(root) = session.emitted_package_path.clone() else {
        return (StatusCode::NOT_FOUND, "session has no emitted package").into_response();
    };
    let Ok(root_canon) = root.canonicalize() else {
        return (StatusCode::NOT_FOUND, "package root missing on disk").into_response();
    };
    let requested = root.join(rel_path.trim_start_matches('/'));
    let Ok(canon) = requested.canonicalize() else {
        return (StatusCode::NOT_FOUND, "artifact not found").into_response();
    };
    if !canon.starts_with(&root_canon) {
        return (StatusCode::FORBIDDEN, "path escapes package root").into_response();
    }
    if !canon.is_file() {
        return (StatusCode::NOT_FOUND, "artifact not a file").into_response();
    }
    let mime = mime_for_path(&canon);
    // Force `attachment` on the dangerous-extension list and
    // stamp `Content-Security-Policy: sandbox` on every artifact response
    // so a same-origin XSS smuggled through an agent-authored artifact
    // can't run script or fetch the SME's session cookie. The route-level
    // sandbox does NOT replace the global CSP from
    // `security_headers_middleware` — both fire (browsers honor the most
    // restrictive policy when multiple CSPs are present).
    let filename = canon
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("artifact");
    let disposition = disposition_for(filename);
    // Stream the artifact via `ReaderStream` rather than reading it
    // whole. A 4 GB BAM under `results/` slurped into a `Vec<u8>` would
    // allocate 4 GB per SME click; the streamed response body pulls
    // 8 KB chunks from a `tokio::fs::File` and writes them to the
    // socket as they arrive — peak server-side memory is
    // O(buffer + small constant) regardless of artifact size.
    let file = match tokio::fs::File::open(&canon).await {
        Ok(f) => f,
        Err(_) => return (StatusCode::NOT_FOUND, "artifact not readable").into_response(),
    };
    let stream = tokio_util::io::ReaderStream::new(file);
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, mime)
        .header(header::CONTENT_DISPOSITION, &disposition)
        .header(header::CONTENT_SECURITY_POLICY, "sandbox")
        .body(Body::from_stream(stream))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

/// SME-authored freeform note on a task.
#[derive(Debug, Deserialize)]
pub struct NoteRequest {
    pub body: String,
    #[serde(default)]
    pub author: Option<String>,
}

/// POST /api/chat/session/:id/task/:task_id/note
///
/// Records a `DecisionType::UserNote` entry on the session. The note
/// is not associated with a turn — it's a side-channel annotation
/// that surfaces in the Decisions tab and the task's drawer.
pub async fn post_task_note(
    State(app): State<ChatAppState>,
    Path((session_id, task_id)): Path<(Uuid, String)>,
    Json(req): Json<NoteRequest>,
) -> impl IntoResponse {
    let body = req.body.trim().to_string();
    if body.is_empty() {
        return (StatusCode::BAD_REQUEST, "note body is empty").into_response();
    }
    if app.conversation.get_session(session_id).await.is_none() {
        return (StatusCode::NOT_FOUND, "session not found").into_response();
    }
    let author = req.author.unwrap_or_default();
    let task_id_for_closure = task_id.clone();
    let store = app.conversation.store_handle();
    let result = store
        .update(session_id, move |s| {
            s.record_decision(
                scripps_workflow_core::decision_log::DecisionType::UserNote {
                    task_id: task_id_for_closure.clone().into(),
                    body: body.clone(),
                    author: author.clone(),
                },
                scripps_workflow_core::decision_log::DecisionActor::Sme,
                None,
            );
            Ok(())
        })
        .await;
    match result {
        Ok(_) => (StatusCode::NO_CONTENT, ()).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("failed to record note: {}", e),
        )
            .into_response(),
    }
}

/// Per-session "stuck task" detector. A task counts as stuck when its
/// state is Running, the heartbeat file exists and is fresh (touched
/// within the last 5 minutes), AND **either** a `*.FAILED` sentinel
/// exists in the task dir **or** no `state.patch.json` (or
/// `state.patch.applied.json`) is present after the heartbeat-stall
/// threshold's worth of time. Surfaces as a banner in the Progress
/// tab so the SME notices an alive-but-unproductive task.
///
/// Polled by the UI every ~30s; cheap (filesystem stat only).
pub async fn get_stuck_tasks(
    State(app): State<ChatAppState>,
    Path(session_id): Path<Uuid>,
) -> impl IntoResponse {
    let Some(session) = app.conversation.get_session(session_id).await else {
        return (StatusCode::NOT_FOUND, "session not found").into_response();
    };
    let Some(pkg) = session.emitted_package_path.clone() else {
        return Json(serde_json::json!({ "stuck": [] })).into_response();
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // Walk WORKFLOW.json for the Running set. Cheaper than wiring
    // a session-side accessor and lives in the same filesystem the
    // detector already polls.
    let running_ids: Vec<String> = match read_running_tasks(&pkg).await {
        Ok(v) => v,
        Err(_) => return Json(serde_json::json!({ "stuck": [] })).into_response(),
    };
    let mut stuck: Vec<serde_json::Value> = Vec::new();
    for tid in &running_ids {
        // Defense in depth: even though `tid` comes from WORKFLOW.json
        // (trusted at intake time), jail it here so a malformed task id
        // can't escape to a sibling directory under the package root.
        let outputs = match super::super::_path_jail::runtime_outputs_for_task(&pkg, tid) {
            Ok(d) => d,
            Err(_) => continue,
        };
        let hb = outputs.join(".heartbeat");
        let Ok(hb_meta) = std::fs::metadata(&hb) else {
            continue;
        };
        let hb_mtime = hb_meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let hb_age = now.saturating_sub(hb_mtime);
        if hb_age > 300 {
            // not stuck — heartbeat-stall path will catch it
            continue;
        }
        // Look for *.FAILED sentinel or absent state.patch.json
        let mut failing_sentinel: Option<String> = None;
        if let Ok(entries) = std::fs::read_dir(&outputs) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                if name.ends_with(".FAILED") {
                    failing_sentinel = Some(name);
                    break;
                }
            }
        }
        let patch_present = outputs.join("state.patch.json").exists()
            || outputs.join("state.patch.applied.json").exists();
        let stale_no_patch = hb_age > 120 && !patch_present && failing_sentinel.is_none();
        if let Some(sentinel) = failing_sentinel.clone() {
            stuck.push(serde_json::json!({
                "task_id": tid,
                "kind": "failed_sentinel_no_transition",
                "reason": format!("agent wrote {} but task is still Running. The wrapper script crashed; agent hasn't yet emitted a state.patch.json.", sentinel),
                "last_heartbeat_unix": hb_mtime,
                "failing_sentinel": sentinel,
            }));
        } else if stale_no_patch {
            stuck.push(serde_json::json!({
                "task_id": tid,
                "kind": "no_patch_after_heartbeat",
                "reason": "Heartbeat fresh but no state.patch.json after 2+ minutes. Task is alive but not producing a transition.".to_string(),
                "last_heartbeat_unix": hb_mtime,
                "failing_sentinel": serde_json::Value::Null,
            }));
        }
    }
    Json(serde_json::json!({ "stuck": stuck })).into_response()
}

/// Read at most `cap` bytes from the tail of `path`. Returns the
/// suffix as a lossy `String` (any non-UTF-8 byte sequences across the
/// seek boundary collapse to `U+FFFD`; we never need byte-perfect
/// fidelity here — the only consumers are line-oriented scans of a
/// human-readable progress log). Missing/unreadable file returns Err.
async fn read_log_tail(path: &std::path::Path, cap: u64) -> std::io::Result<String> {
    use tokio::io::{AsyncSeekExt, SeekFrom};
    let len = tokio::fs::metadata(path).await?.len();
    let mut file = tokio::fs::File::open(path).await?;
    let seek_to = len.saturating_sub(cap);
    if seek_to > 0 {
        file.seek(SeekFrom::Start(seek_to)).await?;
    }
    let mut buf = Vec::new();
    file.take(cap).read_to_end(&mut buf).await?;
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

/// Parse `WORKFLOW.json` and return ids of tasks whose state tag is
/// `running`. Returns Err if WORKFLOW.json is unreadable or unparseable.
///
/// switched to `tokio::fs` so the (polled-every-30s)
/// `get_stuck_tasks` handler doesn't block a tokio worker thread on
/// the WORKFLOW.json read. Caps the WORKFLOW.json slurp at
/// `RESULT_SIDECAR_READ_CAP_BYTES` so a pathologically large workflow
/// (e.g. an `IterateUntil` expansion that overran its convergence
/// bound) can't pin hundreds of MB per polling tick.
async fn read_running_tasks(pkg: &std::path::Path) -> Result<Vec<String>, std::io::Error> {
    let file = tokio::fs::File::open(pkg.join("WORKFLOW.json")).await?;
    let mut buf = Vec::new();
    file.take(RESULT_SIDECAR_READ_CAP_BYTES)
        .read_to_end(&mut buf)
        .await?;
    let value: serde_json::Value = serde_json::from_slice(&buf)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;
    let mut out: Vec<String> = Vec::new();
    if let Some(tasks) = value.get("tasks").and_then(|t| t.as_object()) {
        for (id, task) in tasks {
            let status = task
                .get("state")
                .and_then(|s| s.get("status"))
                .and_then(|s| s.as_str())
                .unwrap_or("");
            if status == "running" {
                out.push(id.clone());
            }
        }
    }
    out.sort();
    Ok(out)
}

// ─────────────────────── Active tasks (per-task progress) ─────────────
//
// Returns one summary per task currently in TaskState::Running, with
// elapsed time, heartbeat freshness, last progress.log line, and a
// determinate-or-indeterminate progress signal. Polled every 2s by the
// UI's RunningTasksPanel; the panel re-derives from this list every
// poll, so a task that transitions Running → Completed/Blocked falls
// out of the response and the corresponding card auto-removes from
// the UI without explicit dismount logic.

#[derive(serde::Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum ActiveTaskProgress {
    /// Concrete N/total. Total comes from
    /// `task.spec.required_figures.len()` (figures present on disk
    /// vs expected) or from `task.spec.expected_artifacts.len()`
    /// when figures aren't declared.
    Determinate {
        completed: u32,
        total: u32,
        unit: String,
    },
    /// No countable signal. UI shows a CSS shimmer animation.
    /// `eta_min_secs`/`eta_max_secs` are optional pilot-report
    /// projections; absent unless `SWFC_PILOT_ENABLED=1` produced
    /// a sizing report for this stage class.
    Indeterminate {
        eta_min_secs: Option<u64>,
        eta_max_secs: Option<u64>,
    },
}

#[derive(serde::Serialize)]
pub(crate) struct ActiveTaskSummary {
    pub task_id: String,
    pub stage_class: String,
    pub friendly_name: String,
    pub started_at: String,
    pub elapsed_secs: u64,
    pub heartbeat_age_secs: Option<u64>,
    pub last_progress_line: Option<String>,
    pub progress: ActiveTaskProgress,
}

/// `GET /api/chat/session/:id/active-tasks` — list tasks currently in Running state with live progress data.
pub async fn get_active_tasks(
    State(app): State<ChatAppState>,
    Path(session_id): Path<Uuid>,
) -> impl IntoResponse {
    let Some(session) = app.conversation.get_session(session_id).await else {
        return (StatusCode::NOT_FOUND, "session not found").into_response();
    };
    let pkg_dir = session.emitted_package_path.clone();
    let dag = match session.current_dag() {
        Some(d) => d,
        None => {
            return Json(serde_json::json!({ "active_tasks": [] })).into_response();
        }
    };

    use scripps_workflow_core::dag::TaskState;
    let now = chrono::Utc::now();
    let mut summaries: Vec<ActiveTaskSummary> = Vec::new();
    let step_re = regex::Regex::new(r"\[\s*[Ss][Tt][Ee][Pp]\s+(\d+)\s*/\s*(\d+)\s*\]").ok();
    for (task_id, task) in dag.tasks.iter() {
        let started_at = match &task.state {
            TaskState::Running { started_at, .. } => started_at.clone(),
            _ => continue,
        };
        let started_dt = chrono::DateTime::parse_from_rfc3339(&started_at)
            .map(|t| t.with_timezone(&chrono::Utc))
            .unwrap_or(now);
        let elapsed = (now - started_dt).num_seconds().max(0) as u64;

        // Stage-class label from task.spec
        let stage_class = task
            .spec
            .as_ref()
            .and_then(|s| s.get("stage_class"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        // Friendly name: use the stage-class title-cased; the front
        // end has a richer stage-descriptions table it can apply
        // client-side, but the server-side label is a reasonable
        // fallback that doesn't require pulling the YAML at request
        // time.
        let friendly_name = if stage_class.is_empty() {
            task_id.to_string()
        } else {
            stage_class
                .split('_')
                .map(|w| {
                    let mut chars = w.chars();
                    match chars.next() {
                        Some(c) => c.to_uppercase().chain(chars).collect::<String>(),
                        None => String::new(),
                    }
                })
                .collect::<Vec<_>>()
                .join(" ")
        };

        // Per-task on-disk reads (heartbeat, progress.log tail,
        // figures manifest). Best-effort — any IO error degrades to
        // None so the endpoint never 500s on a transient fs hiccup.
        let mut heartbeat_age_secs: Option<u64> = None;
        let mut last_progress_line: Option<String> = None;
        let mut figures_completed: Option<u32> = None;
        let mut artifacts_completed: Option<u32> = None;
        // Tier 3 progress signal: parse the most recent `[step N/M]`
        // marker from progress.log. The agent's PROMPT.md instructs
        // it to emit one of these per phase so even tasks without
        // expected_artifacts get determinate progress. Pattern is
        // Intentionally permissive — `[step 4/7]`, `[step 4 / 7 ]`,
        // and `[STEP 04/12]` all match.
        let mut step_progress: Option<(u32, u32)> = None;
        if let Some(pkg) = &pkg_dir {
            let task_dir =
                match super::super::_path_jail::runtime_outputs_for_task(pkg, task_id.as_str()) {
                    Ok(d) => d,
                    Err(_) => continue,
                };
            // Heartbeat
            if let Ok(meta) = tokio::fs::metadata(task_dir.join(".heartbeat")).await {
                if let Ok(modified) = meta.modified() {
                    if let Ok(dur) = std::time::SystemTime::now().duration_since(modified) {
                        heartbeat_age_secs = Some(dur.as_secs());
                    }
                }
            }
            // Last non-empty line of progress.log + most recent
            // `[step N/M]` step marker for Tier 3 determinate signal.
            // Bounded tail-read: open the file, seek to the last
            // `PROGRESS_LOG_TAIL_CAP_BYTES`, and read forward. The
            // harness's append-only contract bounds progress.log to
            // ~1000 lines, but a verbose agent dumping stdout into
            // the same file can blow that; previously the
            // `read_to_string` slurp on every 2s poll of
            // `get_active_tasks` allocated the whole file on each
            // tick. Seeking from the tail keeps memory O(tail cap)
            // regardless of file size; the newest-first scan still
            // wins because the marker we want is the most recent one.
            if let Ok(progress_tail) =
                read_log_tail(&task_dir.join("progress.log"), PROGRESS_LOG_TAIL_CAP_BYTES).await
            {
                last_progress_line = progress_tail
                    .lines()
                    .rev()
                    .find(|l| !l.trim().is_empty())
                    .map(|s| s.to_string());
                // Walk lines newest-first; first match wins. Cheap
                // O(N) scan over the bounded tail window.
                if let Some(re) = &step_re {
                    for line in progress_tail.lines().rev() {
                        if let Some(caps) = re.captures(line) {
                            let n: Option<u32> = caps.get(1).and_then(|m| m.as_str().parse().ok());
                            let m: Option<u32> = caps.get(2).and_then(|m| m.as_str().parse().ok());
                            if let (Some(n), Some(m)) = (n, m) {
                                if m > 0 {
                                    step_progress = Some((n, m));
                                    break;
                                }
                            }
                        }
                    }
                }
            }
            // Figures: count unique figure ids present in `figures/`
            // regardless of which extension(s) the renderer emitted
            // (png/pdf/svg/html/eps/jpeg/webp). Counting only `.png`
            // under-reported progress for renderers that ship vector
            // artifacts; bucket by file stem so a figure with both
            // `.png` and `.pdf` still counts once. Mirrors the
            // all-extensions hashing rule in
            // `core::figure_diff::enumerate_figures`.
            let figures_dir = task_dir.join("figures");
            if let Ok(mut rd) = tokio::fs::read_dir(&figures_dir).await {
                let figure_exts: &[&str] =
                    &["png", "pdf", "svg", "html", "eps", "jpeg", "jpg", "webp"];
                let mut seen_ids: std::collections::BTreeSet<String> =
                    std::collections::BTreeSet::new();
                while let Ok(Some(entry)) = rd.next_entry().await {
                    let p = entry.path();
                    let Some(stem) = p.file_stem().and_then(|s| s.to_str()) else {
                        continue;
                    };
                    if stem == "manifest" {
                        continue;
                    }
                    let Some(ext) = p
                        .extension()
                        .and_then(|e| e.to_str())
                        .map(str::to_ascii_lowercase)
                    else {
                        continue;
                    };
                    if !figure_exts.contains(&ext.as_str()) {
                        continue;
                    }
                    seen_ids.insert(stem.to_string());
                }
                figures_completed = Some(seen_ids.len() as u32);
            }
            // Expected artifacts: count direct files in task_dir that
            // match any of the names in spec.expected_artifacts.
            // Best-effort.
            if let Some(expected) = task
                .spec
                .as_ref()
                .and_then(|s| s.get("expected_artifacts"))
                .and_then(|v| v.as_array())
            {
                let mut n: u32 = 0;
                for e in expected.iter().filter_map(|v| v.as_str()) {
                    if tokio::fs::metadata(task_dir.join(e)).await.is_ok() {
                        n += 1;
                    }
                }
                artifacts_completed = Some(n);
            }
        }

        // Decide determinate vs indeterminate. Prefer required_figures
        // when present + figures dir exists; fall back to expected_artifacts;
        // otherwise indeterminate.
        let progress = (|| {
            // Tier 3 — agent's own [step N/M] marker. Highest priority
            // when present because the agent knows its phases best.
            if let Some((n, m)) = step_progress {
                return ActiveTaskProgress::Determinate {
                    completed: n.min(m),
                    total: m,
                    unit: "steps".to_string(),
                };
            }
            let required_figs = task
                .spec
                .as_ref()
                .and_then(|s| s.get("required_figures"))
                .and_then(|v| v.as_array())
                .map(|a| a.len() as u32);
            if let (Some(total), Some(done)) = (required_figs, figures_completed) {
                if total > 0 {
                    return ActiveTaskProgress::Determinate {
                        completed: done.min(total),
                        total,
                        unit: "figures".to_string(),
                    };
                }
            }
            let expected_arts = task
                .spec
                .as_ref()
                .and_then(|s| s.get("expected_artifacts"))
                .and_then(|v| v.as_array())
                .map(|a| a.len() as u32);
            if let (Some(total), Some(done)) = (expected_arts, artifacts_completed) {
                if total > 0 {
                    return ActiveTaskProgress::Determinate {
                        completed: done.min(total),
                        total,
                        unit: "artifacts".to_string(),
                    };
                }
            }
            ActiveTaskProgress::Indeterminate {
                eta_min_secs: None,
                eta_max_secs: None,
            }
        })();

        summaries.push(ActiveTaskSummary {
            task_id: task_id.to_string(),
            stage_class,
            friendly_name,
            started_at,
            elapsed_secs: elapsed,
            heartbeat_age_secs,
            last_progress_line,
            progress,
        });
    }
    // Sort by started_at ascending so longest-running task is first.
    summaries.sort_by(|a, b| a.started_at.cmp(&b.started_at));

    Json(serde_json::json!({ "active_tasks": summaries })).into_response()
}

pub(crate) fn routes() -> axum::Router<ChatAppState> {
    axum::Router::new()
        .route(
            "/api/chat/session/:id/task/:task_id/result",
            axum::routing::get(get_task_result),
        )
        .route(
            "/api/chat/session/:id/artifacts/*path",
            axum::routing::get(get_artifact),
        )
        .route(
            "/api/chat/session/:id/package.tar.gz",
            axum::routing::get(super::package_download::get_package_tarball),
        )
        .route(
            "/api/chat/session/:id/task/:task_id/note",
            axum::routing::post(post_task_note),
        )
        .route(
            "/api/chat/session/:id/stuck-tasks",
            axum::routing::get(get_stuck_tasks),
        )
        .route(
            "/api/chat/session/:id/active-tasks",
            axum::routing::get(get_active_tasks),
        )
}

#[cfg(test)]
mod artifact_disposition_tests {
    use super::disposition_for;
    #[test]
    fn html_is_attachment() {
        assert_eq!(
            disposition_for("foo.html"),
            "attachment; filename=\"foo.html\""
        );
    }
    #[test]
    fn htm_is_attachment() {
        assert_eq!(
            disposition_for("legacy.htm"),
            "attachment; filename=\"legacy.htm\""
        );
    }
    #[test]
    fn svg_is_attachment() {
        assert_eq!(
            disposition_for("plot.svg"),
            "attachment; filename=\"plot.svg\""
        );
    }
    #[test]
    fn js_is_attachment() {
        assert_eq!(
            disposition_for("bundle.js"),
            "attachment; filename=\"bundle.js\""
        );
    }
    #[test]
    fn mjs_is_attachment() {
        assert_eq!(
            disposition_for("module.mjs"),
            "attachment; filename=\"module.mjs\""
        );
    }
    #[test]
    fn xml_is_attachment() {
        assert_eq!(
            disposition_for("manifest.xml"),
            "attachment; filename=\"manifest.xml\""
        );
    }
    #[test]
    fn xhtml_is_attachment() {
        assert_eq!(
            disposition_for("page.xhtml"),
            "attachment; filename=\"page.xhtml\""
        );
    }
    #[test]
    fn wasm_is_attachment() {
        assert_eq!(
            disposition_for("kernel.wasm"),
            "attachment; filename=\"kernel.wasm\""
        );
    }
    #[test]
    fn png_is_inline() {
        assert_eq!(disposition_for("plot.png"), "inline; filename=\"plot.png\"");
    }
    #[test]
    fn tsv_is_inline() {
        assert_eq!(
            disposition_for("table.tsv"),
            "inline; filename=\"table.tsv\""
        );
    }
    #[test]
    fn case_insensitive_extension() {
        assert_eq!(
            disposition_for("INDEX.HTML"),
            "attachment; filename=\"INDEX.HTML\""
        );
    }
    #[test]
    fn no_extension_is_inline() {
        assert_eq!(disposition_for("README"), "inline; filename=\"README\"");
    }
}

#[cfg(test)]
mod tests {
    use super::scan_artifacts;
    use crate::chat_routes::test_support::{
        body_json, make_router, seed_session_with_completed_task,
    };
    use axum::body::{to_bytes, Body};
    use axum::http::{Request, StatusCode};
    use tower::util::ServiceExt;
    use uuid::Uuid;

    #[test]
    fn result_handler_rejects_traversal_task_id() {
        let tmp = tempfile::tempdir().unwrap();
        let pkg = tmp.path();
        // Direct check of the resolver — handler-level integration covered
        // by the existing chat_routes integration tests.
        assert!(super::super::super::_path_jail::runtime_outputs_for_task(pkg, "../etc").is_err());
    }

    #[test]
    fn scan_artifacts_rejects_traversal_task_id() {
        let tmp = tempfile::tempdir().unwrap();
        let pkg = tmp.path();
        std::fs::create_dir_all(pkg.join("runtime/legit")).unwrap();
        // Traversal task_id returns an empty list, not arbitrary file reads.
        let out = scan_artifacts(pkg, "../etc");
        assert!(out.is_empty());
    }

    #[tokio::test]
    async fn task_result_returns_completed_task_with_empty_artifacts_when_no_package() {
        let (router, app) = make_router(vec![]).await;
        let id = seed_session_with_completed_task(&app, "t_demo", None).await;

        let req = Request::builder()
            .method("GET")
            .uri(format!("/api/chat/session/{}/task/t_demo/result", id))
            .body(Body::empty())
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp.into_body()).await;
        assert_eq!(body["task_id"], "t_demo");
        assert_eq!(body["status"], "completed");
        assert_eq!(body["description"], "demo completed task");
        assert!(body["result"].is_object());
        let arts = body["artifacts"].as_array().expect("artifacts array");
        assert!(arts.is_empty(), "no package → no artifacts");
    }

    #[tokio::test]
    async fn task_result_404_when_session_missing() {
        let (router, _) = make_router(vec![]).await;
        let bogus = Uuid::new_v4();
        let req = Request::builder()
            .method("GET")
            .uri(format!("/api/chat/session/{}/task/whatever/result", bogus))
            .body(Body::empty())
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn task_result_404_when_task_missing() {
        let (router, app) = make_router(vec![]).await;
        let id = seed_session_with_completed_task(&app, "t_demo", None).await;

        let req = Request::builder()
            .method("GET")
            .uri(format!("/api/chat/session/{}/task/not_a_task/result", id))
            .body(Body::empty())
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn task_result_lists_artifacts_from_package_directory() {
        let pkg = tempfile::tempdir().unwrap();
        let runtime_dir = pkg.path().join("runtime").join("t_demo");
        std::fs::create_dir_all(&runtime_dir).unwrap();
        std::fs::write(runtime_dir.join("plot.png"), b"\x89PNG\x0d\x0afake").unwrap();
        std::fs::write(runtime_dir.join("summary.html"), "<html>hi</html>").unwrap();

        let (router, app) = make_router(vec![]).await;
        let id =
            seed_session_with_completed_task(&app, "t_demo", Some(pkg.path().to_path_buf())).await;

        let req = Request::builder()
            .method("GET")
            .uri(format!("/api/chat/session/{}/task/t_demo/result", id))
            .body(Body::empty())
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp.into_body()).await;
        let arts = body["artifacts"].as_array().expect("artifacts array");
        assert_eq!(arts.len(), 2);
        let by_name: std::collections::BTreeMap<String, &serde_json::Value> = arts
            .iter()
            .map(|a| (a["name"].as_str().unwrap().to_string(), a))
            .collect();
        let plot = by_name.get("plot.png").expect("plot.png present");
        assert_eq!(plot["mime_type"], "image/png");
        assert_eq!(plot["relative_path"], "runtime/t_demo/plot.png");
        assert!(plot["size_bytes"].as_u64().unwrap() > 0);
        let html = by_name.get("summary.html").expect("summary.html present");
        assert_eq!(html["mime_type"], "text/html; charset=utf-8");
        drop(pkg);
    }

    #[tokio::test]
    async fn artifact_get_serves_file_inside_package_root() {
        let pkg = tempfile::tempdir().unwrap();
        let runtime_dir = pkg.path().join("runtime").join("t_demo");
        std::fs::create_dir_all(&runtime_dir).unwrap();
        let body_bytes = b"col_a\tcol_b\n1\t2\n";
        std::fs::write(runtime_dir.join("table.tsv"), body_bytes).unwrap();

        let (router, app) = make_router(vec![]).await;
        let id =
            seed_session_with_completed_task(&app, "t_demo", Some(pkg.path().to_path_buf())).await;

        let req = Request::builder()
            .method("GET")
            .uri(format!(
                "/api/chat/session/{}/artifacts/runtime/t_demo/table.tsv",
                id
            ))
            .body(Body::empty())
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            resp.headers()
                .get("content-type")
                .and_then(|v| v.to_str().ok()),
            Some("text/tab-separated-values")
        );
        // Every artifact response carries CSP sandbox, and
        // inline-renderable safe types still get a `inline; filename=…`
        // disposition (only the dangerous-extension list flips to
        // `attachment`).
        assert_eq!(
            resp.headers()
                .get("content-disposition")
                .and_then(|v| v.to_str().ok()),
            Some("inline; filename=\"table.tsv\"")
        );
        assert_eq!(
            resp.headers()
                .get("content-security-policy")
                .and_then(|v| v.to_str().ok()),
            Some("sandbox")
        );
        let bytes = to_bytes(resp.into_body(), 1_000_000).await.unwrap();
        assert_eq!(bytes.as_ref(), body_bytes);
        drop(pkg);
    }

    #[tokio::test]
    async fn artifact_html_is_forced_attachment_with_csp_sandbox() {
        // An artifact HTML file must NOT
        // render in-browser. Force `Content-Disposition: attachment` +
        // `Content-Security-Policy: sandbox` so a same-origin XSS via an
        // adversarial agent artifact can't run script in the SME's
        // session.
        let pkg = tempfile::tempdir().unwrap();
        let runtime_dir = pkg.path().join("runtime").join("t_demo");
        std::fs::create_dir_all(&runtime_dir).unwrap();
        let body_bytes = b"<html><body><script>alert(1)</script></body></html>";
        std::fs::write(runtime_dir.join("attack.html"), body_bytes).unwrap();

        let (router, app) = make_router(vec![]).await;
        let id =
            seed_session_with_completed_task(&app, "t_demo", Some(pkg.path().to_path_buf())).await;

        let req = Request::builder()
            .method("GET")
            .uri(format!(
                "/api/chat/session/{}/artifacts/runtime/t_demo/attack.html",
                id
            ))
            .body(Body::empty())
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            resp.headers()
                .get("content-disposition")
                .and_then(|v| v.to_str().ok()),
            Some("attachment; filename=\"attack.html\"")
        );
        assert_eq!(
            resp.headers()
                .get("content-security-policy")
                .and_then(|v| v.to_str().ok()),
            Some("sandbox")
        );
        drop(pkg);
    }

    #[tokio::test]
    async fn artifact_svg_is_forced_attachment() {
        // SVG can host inline `<script>` and is therefore in the
        // dangerous-extension list.
        let pkg = tempfile::tempdir().unwrap();
        let runtime_dir = pkg.path().join("runtime").join("t_demo");
        std::fs::create_dir_all(&runtime_dir).unwrap();
        std::fs::write(
            runtime_dir.join("plot.svg"),
            br#"<svg xmlns="http://www.w3.org/2000/svg"><script>alert(1)</script></svg>"#,
        )
        .unwrap();

        let (router, app) = make_router(vec![]).await;
        let id =
            seed_session_with_completed_task(&app, "t_demo", Some(pkg.path().to_path_buf())).await;

        let req = Request::builder()
            .method("GET")
            .uri(format!(
                "/api/chat/session/{}/artifacts/runtime/t_demo/plot.svg",
                id
            ))
            .body(Body::empty())
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            resp.headers()
                .get("content-disposition")
                .and_then(|v| v.to_str().ok()),
            Some("attachment; filename=\"plot.svg\"")
        );
        drop(pkg);
    }

    #[tokio::test]
    async fn artifact_get_rejects_traversal() {
        let pkg = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(pkg.path().join("runtime/t_demo")).unwrap();
        std::fs::write(pkg.path().join("runtime/t_demo/x.txt"), "hi").unwrap();
        let outside_parent = pkg.path().parent().unwrap();
        let outside_file = outside_parent.join("outside-secret.txt");
        std::fs::write(&outside_file, "leaked").unwrap();

        let (router, app) = make_router(vec![]).await;
        let id =
            seed_session_with_completed_task(&app, "t_demo", Some(pkg.path().to_path_buf())).await;

        let req = Request::builder()
            .method("GET")
            .uri(format!(
                "/api/chat/session/{}/artifacts/..%2Foutside-secret.txt",
                id
            ))
            .body(Body::empty())
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert!(
            !resp.status().is_success(),
            "traversal must not return 2xx, got {}",
            resp.status()
        );
        let _ = std::fs::remove_file(&outside_file);
        drop(pkg);
    }

    #[tokio::test]
    async fn artifact_get_404_when_session_has_no_package() {
        let (router, app) = make_router(vec![]).await;
        let id = seed_session_with_completed_task(&app, "t_demo", None).await;
        let req = Request::builder()
            .method("GET")
            .uri(format!("/api/chat/session/{}/artifacts/anything.txt", id))
            .body(Body::empty())
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn artifact_get_rejects_raw_dotdot_traversal() {
        // Variant of artifact_get_rejects_traversal that exercises raw `..`
        // path segments rather than `..%2F`. The canonicalize-then-
        // starts_with guard should reject regardless of whether axum's
        // wildcard captures the segments literally or after normalization.
        let pkg = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(pkg.path().join("runtime/t_demo")).unwrap();
        let outside_parent = pkg.path().parent().unwrap();
        let outside_file = outside_parent.join("outside-secret-raw.txt");
        std::fs::write(&outside_file, "leaked").unwrap();

        let (router, app) = make_router(vec![]).await;
        let id =
            seed_session_with_completed_task(&app, "t_demo", Some(pkg.path().to_path_buf())).await;

        let req = Request::builder()
            .method("GET")
            .uri(format!(
                "/api/chat/session/{}/artifacts/runtime/t_demo/../../outside-secret-raw.txt",
                id
            ))
            .body(Body::empty())
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert!(
            !resp.status().is_success(),
            "raw `../` traversal must not return 2xx, got {}",
            resp.status()
        );
        let _ = std::fs::remove_file(&outside_file);
        drop(pkg);
    }

    #[tokio::test]
    async fn artifact_get_rejects_double_encoded_traversal() {
        // `%25` decodes to `%`, so `%252e%252e%2F` decodes once to
        // `%2e%2e/`. Defenses that decode twice would resolve this to
        // `../`. Our handler must NOT do that — assert non-success.
        let pkg = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(pkg.path().join("runtime/t_demo")).unwrap();

        let (router, app) = make_router(vec![]).await;
        let id =
            seed_session_with_completed_task(&app, "t_demo", Some(pkg.path().to_path_buf())).await;

        let req = Request::builder()
            .method("GET")
            .uri(format!(
                "/api/chat/session/{}/artifacts/%252e%252e%2Fpasswd",
                id
            ))
            .body(Body::empty())
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert!(
            !resp.status().is_success(),
            "double-encoded traversal must not return 2xx, got {}",
            resp.status()
        );
        drop(pkg);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn artifact_get_rejects_symlink_targeting_outside_root() {
        // Create a symlink inside the package root that points at a file
        // *outside* the root. canonicalize() resolves the symlink, the
        // resolved path no longer starts_with(root), and the guard fires
        // 403. This catches the symlink attack the URL-encoded test
        // doesn't exercise.
        use std::os::unix::fs::symlink;
        let pkg = tempfile::tempdir().unwrap();
        let task_dir = pkg.path().join("runtime/t_demo");
        std::fs::create_dir_all(&task_dir).unwrap();
        // Real file outside the package root.
        let outside_file = pkg.path().parent().unwrap().join("outside-via-symlink.txt");
        std::fs::write(&outside_file, "leaked").unwrap();
        // Symlink inside the package root pointing at the outside file.
        let link_path = task_dir.join("leak");
        symlink(&outside_file, &link_path).unwrap();

        let (router, app) = make_router(vec![]).await;
        let id =
            seed_session_with_completed_task(&app, "t_demo", Some(pkg.path().to_path_buf())).await;

        let req = Request::builder()
            .method("GET")
            .uri(format!(
                "/api/chat/session/{}/artifacts/runtime/t_demo/leak",
                id
            ))
            .body(Body::empty())
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        // After canonicalize() resolves the symlink, starts_with(root)
        // fails → FORBIDDEN. Anything in the !success range is OK.
        assert!(
            !resp.status().is_success(),
            "symlink-out-of-root must not return 2xx, got {}",
            resp.status()
        );
        let _ = std::fs::remove_file(&link_path);
        let _ = std::fs::remove_file(&outside_file);
        drop(pkg);
    }

    #[tokio::test]
    async fn task_result_surfaces_agent_code_when_sidecar_present() {
        // Write a minimal agent-code.json sidecar under
        // runtime/outputs/<task_id>/ and confirm the endpoint attaches it.
        let pkg = tempfile::tempdir().unwrap();
        let runtime_dir = pkg.path().join("runtime").join("outputs").join("t_demo");
        std::fs::create_dir_all(&runtime_dir).unwrap();
        let rec = scripps_workflow_core::agent_code::AgentCodeRecord::new(
            "my prompt".to_string(),
            "2026-05-22T10:00:00Z".to_string(),
            "2026-05-22T10:05:00Z".to_string(),
        );
        std::fs::write(
            runtime_dir.join("agent-code.json"),
            serde_json::to_vec(&rec).unwrap(),
        )
        .unwrap();

        let (router, app) = make_router(vec![]).await;
        let id =
            seed_session_with_completed_task(&app, "t_demo", Some(pkg.path().to_path_buf())).await;

        let req = axum::http::Request::builder()
            .method("GET")
            .uri(format!("/api/chat/session/{}/task/t_demo/result", id))
            .body(axum::body::Body::empty())
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), axum::http::StatusCode::OK);
        let body = body_json(resp.into_body()).await;
        let agent_code = &body["agent_code"];
        assert!(
            !agent_code.is_null(),
            "agent_code must be present when sidecar exists"
        );
        assert_eq!(agent_code["prompt"], "my prompt");
        assert_eq!(agent_code["started_at"], "2026-05-22T10:00:00Z");
        assert_eq!(agent_code["completed_at"], "2026-05-22T10:05:00Z");
        assert_eq!(agent_code["language"], "unknown");
        drop(pkg);
    }

    #[tokio::test]
    async fn task_result_agent_code_null_when_sidecar_absent() {
        // No sidecar written — agent_code field must be null (not an error).
        let pkg = tempfile::tempdir().unwrap();
        let runtime_dir = pkg.path().join("runtime").join("outputs").join("t_demo");
        std::fs::create_dir_all(&runtime_dir).unwrap();
        // Intentionally no agent-code.json written.

        let (router, app) = make_router(vec![]).await;
        let id =
            seed_session_with_completed_task(&app, "t_demo", Some(pkg.path().to_path_buf())).await;

        let req = axum::http::Request::builder()
            .method("GET")
            .uri(format!("/api/chat/session/{}/task/t_demo/result", id))
            .body(axum::body::Body::empty())
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), axum::http::StatusCode::OK);
        let body = body_json(resp.into_body()).await;
        assert!(
            body["agent_code"].is_null(),
            "agent_code must be null when sidecar is absent"
        );
        drop(pkg);
    }

    #[tokio::test]
    async fn artifact_cache_repopulates_only_when_dir_mtime_changes() {
        // back-to-back `get_task_result` requests for the same
        // completed task should hit the cache on the second call.
        let (_router, app) = make_router(vec![]).await;
        let session_id = Uuid::new_v4();

        assert_eq!(app.artifact_cache.len(), 0);

        app.invalidate_artifact_cache_for_task("task-not-present")
            .await;
        assert_eq!(app.artifact_cache.len(), 0);

        app.artifact_cache.insert(
            (session_id, "alignment".to_string()),
            (123_456_789, 999_999, vec![]),
        );
        app.artifact_cache.insert(
            (session_id, "other_task".to_string()),
            (111_111_111, 999_999, vec![]),
        );
        app.invalidate_artifact_cache_for_task("alignment").await;
        assert_eq!(app.artifact_cache.len(), 1);
        assert!(app
            .artifact_cache
            .contains_key(&(session_id, "other_task".to_string())));
    }
}
