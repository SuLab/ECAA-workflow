//! Routes for the typed remediation surface.
//!
//! Two endpoints:
//!
//! * `GET /api/chat/session/:id/task/:task_id/remediation-suggestions`
//!   — reads `runtime/outputs/<task_id>/error.json`, runs the proposer
//!   side-call (Opus 4.8) on cache miss, returns ranked
//!   `Vec<RemediationSuggestion>`. Cached on `(session, task,
//! envelope.captured_at)` to keep re-fetches free; a fresh
//!   envelope (different captured_at) always re-proposes.
//!
//! * `POST /api/chat/session/:id/task/:task_id/apply-remediation`
//!   — accepts a suggestion id, validates against the cached
//!   proposal set, merges into
//!   `runtime/inputs/<task_id>/overrides.json`, transitions the
//!   session out of `Blocked`, and fires the existing
//!   `maybe_auto_relaunch_harness` debouncer.
//!
//! The closed 16-tool LLM vocabulary is unchanged — apply is a
//! deterministic server action triggered by the SME clicking a button
//! in the BlockerCard, not an LLM-driven mutation.

use super::{BoundedJson, ChatAppState, ExecutionHandle, LlmRateBuckets};
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use ecaa_workflow_conversation::SessionId;
use ecaa_workflow_core::error_envelope::ToolErrorEnvelope;
use ecaa_workflow_core::remediation::{
    ExecutorOverrides, RemediationKind, RemediationSuggestion, ToolBinding,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

const CACHE_KEY_VERSION: &str = "v1";

/// In-memory cache of proposer outputs keyed by
/// `(session, task, envelope_captured_at)`. A fresh envelope changes
/// `captured_at`, naturally invalidating the entry. Bounded by the
/// proposer's MAX_REMEDIATION_ATTEMPTS cap; a runaway session would
/// produce ≤ 5 entries before short-circuiting.
pub(super) type ProposerCache = tokio::sync::RwLock<
    std::collections::HashMap<(SessionId, String, String), Vec<RemediationSuggestion>>,
>;

/// Helper extension for `ChatAppState` so the router can route the new
/// endpoints without breaking existing constructors. The cache itself
/// is constructed lazily via `OnceLock` on first access.
pub(super) fn cache() -> &'static Arc<ProposerCache> {
    use std::sync::OnceLock;
    static CACHE: OnceLock<Arc<ProposerCache>> = OnceLock::new();
    CACHE.get_or_init(|| Arc::new(tokio::sync::RwLock::new(Default::default())))
}

#[derive(Debug, Clone, Serialize)]
pub(super) struct SuggestionsResponse {
    pub envelope: ToolErrorEnvelope,
    pub suggestions: Vec<RemediationSuggestion>,
    pub attempts_consumed: u32,
    /// True when a fresh proposer call ran for this fetch. False when
    /// served from cache. UIs can show "regenerated" subtly.
    pub regenerated: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub(super) struct ApplyRemediationRequest {
    pub suggestion_id: String,
    /// Free-form rationale recorded on the audit trail. Optional.
    #[serde(default)]
    pub rationale: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub(super) struct ApplyRemediationResponse {
    pub suggestion_id: String,
    pub tool_binding: ToolBinding,
    /// `applied` = the override + state transition + auto-relaunch
    /// completed. `guidance_only` = the binding requires the SME to
    /// use a different UI affordance (amend method, set intake
    /// field) — the apply endpoint records the suggestion in the audit
    /// trail but doesn't dispatch the action.
    pub outcome: ApplyOutcome,
    pub message: String,
    pub overrides_path: String,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum ApplyOutcome {
    Applied,
    GuidanceOnly,
}

pub(super) async fn get_remediation_suggestions(
    State(app): State<ChatAppState>,
    Path((session_id, task_id)): Path<(uuid::Uuid, String)>,
) -> impl IntoResponse {
    // remediation-suggestions endpoint fires an Opus 4.8 side-call on
    // cache miss (~$0.05/invocation). Cap at 6/min per session — well
    // above the MAX_REMEDIATION_ATTEMPTS=5 ceiling so legitimate
    // retries are never blocked, but tight enough that a refresh loop
    // can't bypass the cache.
    if let Err(status) = LlmRateBuckets::check(
        &app.llm_buckets.remediation,
        session_id,
        app.llm_rate_limits.remediation,
    ) {
        return (
            status,
            "rate limit exceeded: /remediation-suggestions capped at 6/min/session",
        )
            .into_response();
    }

    let Some(session) = app.conversation.get_session(session_id).await else {
        return (StatusCode::NOT_FOUND, "session not found").into_response();
    };
    let Some(package_path) = session.emitted_package_path.clone() else {
        return (StatusCode::CONFLICT, "session has not emitted a package").into_response();
    };

    let envelope = match read_envelope(&package_path, &task_id) {
        Ok(Some(e)) => e,
        Ok(None) => {
            // No ToolError envelope. Widen the trigger to cover
            // `BlockerKind::ValidationFailed`: a task re-blocked by the
            // validation-contract check (or the on-completion claim
            // verifier) has no `error.json`, but the proposer is still
            // useful — its method-neutral rubric can suggest how to
            // approach the failed domain-correctness check. Synthesize a
            // minimal envelope from the blocked reason + the neutral
            // domain-correctness signal so the existing proposer path
            // applies unchanged. NOTE: the proposer is a LIVE server-side
            // Opus side-call; the headless unattended harness path does
            // NOT use it. Recovery there consumes the deterministic
            // `runtime/inputs/<task>/domain-correctness-signal.json`
            // written by the harness — no LLM call, no server. This
            // endpoint is the SME-facing analog (the SME clicks "suggest"
            // in the BlockerCard).
            match synthesize_validation_failed_envelope(&session, &package_path, &task_id) {
                Some(e) => e,
                None => {
                    return (
                        StatusCode::NOT_FOUND,
                        "no error envelope and no validation-contract block for this task — task may not have failed yet",
                    )
                        .into_response();
                }
            }
        }
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("reading error.json: {e:#}"),
            )
                .into_response();
        }
    };

    let cache_key = (
        session_id,
        task_id.clone(),
        format!("{}:{}", CACHE_KEY_VERSION, envelope.captured_at),
    );

    if let Some(cached) = cache().read().await.get(&cache_key).cloned() {
        let attempts = read_overrides_attempts(&package_path, &task_id);
        return Json(SuggestionsResponse {
            envelope,
            suggestions: cached,
            attempts_consumed: attempts,
            regenerated: false,
        })
        .into_response();
    }

    let prior_attempts = read_overrides(&package_path, &task_id)
        .map(|o| o.history)
        .unwrap_or_default();
    let stage_description = session
        .dag
        .as_ref()
        .and_then(|d| d.tasks.get(task_id.as_str()).cloned())
        .map(|t| t.description);

    let ctx = ecaa_workflow_conversation::side_calls::remediation_proposer::ProposerContext {
        stage_description,
        intake_summary: None,
        prior_attempts,
    };
    let suggestions =
        match ecaa_workflow_conversation::side_calls::remediation_proposer::propose_remediations(
            app.conversation.llm_for_scoring(),
            app.conversation.metrics(),
            session_id,
            &envelope,
            &ctx,
        )
        .await
        {
            Ok(s) => s,
            Err(e) => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("proposer failed: {e:#}"),
                )
                    .into_response();
            }
        };

    cache().write().await.insert(cache_key, suggestions.clone());

    let attempts = read_overrides_attempts(&package_path, &task_id);
    Json(SuggestionsResponse {
        envelope,
        suggestions,
        attempts_consumed: attempts,
        regenerated: true,
    })
    .into_response()
}

pub(super) async fn post_apply_remediation(
    State(app): State<ChatAppState>,
    Path((session_id, task_id)): Path<(uuid::Uuid, String)>,
    BoundedJson(req): BoundedJson<ApplyRemediationRequest>,
) -> impl IntoResponse {
    let Some(session) = app.conversation.get_session(session_id).await else {
        return (StatusCode::NOT_FOUND, "session not found").into_response();
    };
    let Some(package_path) = session.emitted_package_path.clone() else {
        return (StatusCode::CONFLICT, "session has not emitted a package").into_response();
    };

    let envelope = match read_envelope(&package_path, &task_id) {
        Ok(Some(e)) => e,
        Ok(None) => {
            return (StatusCode::NOT_FOUND, "no error envelope for this task").into_response();
        }
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("reading error.json: {e:#}"),
            )
                .into_response();
        }
    };

    let cache_key = (
        session_id,
        task_id.clone(),
        format!("{}:{}", CACHE_KEY_VERSION, envelope.captured_at),
    );
    let suggestion = match cache().read().await.get(&cache_key).cloned() {
        Some(list) => list.into_iter().find(|s| s.id == req.suggestion_id),
        None => None,
    };
    let Some(suggestion) = suggestion else {
        return (
            StatusCode::NOT_FOUND,
            "suggestion id not found in current proposal set; refresh suggestions first",
        )
            .into_response();
    };

    let mut overrides = read_overrides(&package_path, &task_id).unwrap_or_default();
    let applied_at = ecaa_workflow_core::time_helpers::now_rfc3339();
    let applied_by = "sme".to_string();
    if let Err(e) = overrides.merge(&suggestion, applied_at, applied_by) {
        return (
            StatusCode::TOO_MANY_REQUESTS,
            format!("remediation cap reached: {e}"),
        )
            .into_response();
    }
    if let Some(reason) = req.rationale.as_ref() {
        // C-8/C-9: the SME-provided rationale flows into
        // an env_passthrough value that downstream SSM RunCommand /
        // sbatch `--export=` invocations will quote into a shell-
        // interpolated command. Refuse newlines, commas, equals signs,
        // and NULs that could break the envelope shape.
        if !ecaa_workflow_core::env_validator::is_safe_env_value(reason) {
            return (
                StatusCode::BAD_REQUEST,
                "rationale contains characters unsafe for env_passthrough (newline / comma / equals / NUL)",
            )
                .into_response();
        }
        overrides
            .env_passthrough
            .entry("ECAA_REMEDIATION_RATIONALE".into())
            .or_insert(reason.clone());
    }
    // C-8/C-9 final gate: refuse the entire request if any merged
    // override entry would yield an unsafe envelope. The harness
    // executors silently drop invalid entries; rejecting here gives
    // the SME a 400 with an actionable message instead of letting the
    // override write succeed and then silently no-op at dispatch.
    for (lib, ver) in &overrides.library_pins {
        if ecaa_workflow_core::env_validator::sanitize_lib_env_suffix(lib).is_none() {
            return (
                StatusCode::BAD_REQUEST,
                format!("library_pins key {lib:?} is not a valid library identifier (POSIX env-name rules apply after `-`/`.` → `_` normalization)"),
            )
                .into_response();
        }
        if !ecaa_workflow_core::env_validator::is_safe_env_value(ver) {
            return (
                StatusCode::BAD_REQUEST,
                format!("library_pins value for {lib:?} contains characters unsafe for env passthrough (newline / comma / equals / NUL)"),
            )
                .into_response();
        }
    }
    for (k, v) in &overrides.env_passthrough {
        if !ecaa_workflow_core::env_validator::is_valid_env_name(k) {
            return (
                StatusCode::BAD_REQUEST,
                format!("env_passthrough key {k:?} is not a valid POSIX env name"),
            )
                .into_response();
        }
        if !ecaa_workflow_core::env_validator::is_safe_env_value(v) {
            return (
                StatusCode::BAD_REQUEST,
                format!("env_passthrough value for {k:?} contains characters unsafe for env passthrough (newline / comma / equals / NUL)"),
            )
                .into_response();
        }
    }
    if let Err(e) = write_overrides(&package_path, &task_id, &overrides) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("writing overrides.json: {e:#}"),
        )
            .into_response();
    }

    // Route the overrides path through the jail-aware resolver.
    // If task_id is invalid here we'd have errored on the earlier
    // write_overrides call, but reaffirm the contract.
    let overrides_path = match resolve_overrides_path(&package_path, &task_id) {
        Ok(p) => p.display().to_string(),
        Err(_) => {
            return (StatusCode::BAD_REQUEST, "invalid task_id in path").into_response();
        }
    };

    let (outcome, message) = match suggestion.tool_binding {
        ToolBinding::RerunTask | ToolBinding::RerunUpstreamTask => {
            // Drop the proposer cache for this envelope — once we've
            // applied, the next attempt will produce a fresh
            // envelope with a different captured_at anyway, but
            // dropping here keeps the in-memory map bounded.
            cache().write().await.remove(&cache_key);
            // Resolve the producer task id for the upstream variant.
            let target_task: String = match (&suggestion.kind, &suggestion.tool_binding) {
                (
                    RemediationKind::RerunUpstream {
                        producer_task_id, ..
                    },
                    ToolBinding::RerunUpstreamTask,
                ) => producer_task_id.to_string(),
                _ => task_id.clone(),
            };
            // Best-effort unblock — silently no-ops on non-Blocked
            // sessions per the existing transition guard.
            if let Err(e) = app
                .conversation
                .unblock_with_rationale(session_id, req.rationale.clone())
                .await
            {
                eprintln!("[remediation] unblock failed: {e}");
            }
            if let Err(e) = app
                .conversation
                .rerun_task_from_rest(
                    session_id,
                    target_task.clone(),
                    Some(format!(
                        "remediation:{} ({})",
                        suggestion.id,
                        kind_label(&suggestion.kind)
                    )),
                )
                .await
            {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("rerun_task_from_rest failed: {e}"),
                )
                    .into_response();
            }
            super::execution::maybe_auto_relaunch_harness(&app, session_id, "apply_remediation")
                .await;
            (
                ApplyOutcome::Applied,
                format!(
                    "Applied {} and reran task {target_task}.",
                    kind_label(&suggestion.kind)
                ),
            )
        }
        ToolBinding::AmendStageMethod => match &suggestion.kind {
            RemediationKind::SwitchMethod { stage_id, to, .. } => {
                cache().write().await.remove(&cache_key);
                if let Err(e) = app
                    .conversation
                    .unblock_with_rationale(session_id, req.rationale.clone())
                    .await
                {
                    eprintln!("[remediation] unblock failed: {e}");
                }
                match app
                    .conversation
                    .amend_stage_method_from_rest(
                        session_id,
                        stage_id.to_string(),
                        to.clone(),
                        req.rationale
                            .clone()
                            .or_else(|| Some(format!("remediation:{} (switch_method)", suggestion.id))),
                    )
                    .await
                {
                    Ok(_) => {
                        super::execution::maybe_auto_relaunch_harness(
                            &app,
                            session_id,
                            "apply_remediation",
                        )
                        .await;
                        (
                            ApplyOutcome::Applied,
                            format!("Amended {stage_id} to '{to}' and re-emitted the package."),
                        )
                    }
                    Err(e) => {
                        return (
                            StatusCode::INTERNAL_SERVER_ERROR,
                            format!("amend_stage_method_from_rest failed: {e}"),
                        )
                            .into_response();
                    }
                }
            }
            _ => (
                ApplyOutcome::GuidanceOnly,
                "Method swap recorded but variant payload didn't carry a stage/to pair to dispatch."
                    .to_string(),
            ),
        },
        ToolBinding::SetIntakeField => (
            ApplyOutcome::GuidanceOnly,
            "Input swap recorded — use the BlockerCard's Edit-intake affordance to dispatch."
                .to_string(),
        ),
        ToolBinding::OperatorAction => (
            ApplyOutcome::GuidanceOnly,
            "Operator action required — see suggestion rationale for the command to run."
                .to_string(),
        ),
        ToolBinding::ManualOnly => (
            ApplyOutcome::GuidanceOnly,
            "Manual review only — no auto-apply path.".to_string(),
        ),
    };

    Json(ApplyRemediationResponse {
        suggestion_id: suggestion.id,
        tool_binding: suggestion.tool_binding,
        outcome,
        message,
        overrides_path,
    })
    .into_response()
}

/// Resolve `<pkg>/runtime/inputs/<task_id>/overrides.json` with the
/// path jail applied to `task_id`.
fn resolve_overrides_path(
    pkg: &std::path::Path,
    task_id: &str,
) -> Result<std::path::PathBuf, StatusCode> {
    let inputs_base = pkg.join("runtime/inputs");
    let task_inputs = super::_path_jail::safe_segment_join(&inputs_base, task_id)
        .map_err(|_| StatusCode::BAD_REQUEST)?;
    let overrides = task_inputs.join("overrides.json");
    super::_path_jail::assert_under_root(pkg, &overrides).map_err(|_| StatusCode::BAD_REQUEST)?;
    Ok(overrides)
}

/// Resolve `<pkg>/runtime/outputs/<task_id>/error.json` with the path
/// jail applied to `task_id`.
fn resolve_envelope_path(
    pkg: &std::path::Path,
    task_id: &str,
) -> Result<std::path::PathBuf, StatusCode> {
    let task_dir = super::_path_jail::runtime_outputs_for_task(pkg, task_id)
        .map_err(|_| StatusCode::BAD_REQUEST)?;
    Ok(task_dir.join("error.json"))
}

/// Markers a harness / verifier writes into a `BlockedRecord::reason`
/// for a `BlockerKind::ValidationFailed`-class block. Matching any of
/// these makes the task eligible for a synthesized envelope so the
/// proposer trigger covers validation failures, not just tool errors.
const VALIDATION_BLOCK_MARKERS: &[&str] = &[
    "[validation_failed]",
    "validation-contract",
    "required assertion",
    "ValidationFailed",
];

/// Synthesize a minimal [`ToolErrorEnvelope`] for a task that a
/// validation-contract / claim-verifier block left Blocked (no
/// `error.json` present). Returns `None` when the task is not Blocked on
/// a validation-failure reason, so the caller falls through to the
/// "nothing to remediate" 404.
///
/// The synthesized envelope's `stderr` carries the blocked reason plus
/// the METHOD-NEUTRAL domain-correctness statements from
/// `runtime/inputs/<task>/domain-correctness-signal.json` when present
/// (written by the harness autonomous-recovery path). Those statements
/// already name no tool/flag/threshold, and the proposer prompt's
/// method-neutrality rubric is unchanged, so widening the trigger does
/// not loosen neutrality.
fn synthesize_validation_failed_envelope(
    session: &ecaa_workflow_conversation::session::Session,
    package: &std::path::Path,
    task_id: &str,
) -> Option<ToolErrorEnvelope> {
    use ecaa_workflow_core::dag::TaskState;

    let dag = session.dag.as_ref()?;
    let task = dag.tasks.get(task_id)?;
    let reason = match &task.state {
        TaskState::Blocked { record } => record.reason.clone(),
        _ => return None,
    };
    if !VALIDATION_BLOCK_MARKERS
        .iter()
        .any(|m| reason.contains(m))
    {
        return None;
    }

    // Pull the neutral domain-correctness statements (if the harness
    // wrote them) so the proposer sees exactly what is biologically off.
    let neutral_signal = read_domain_correctness_statements(package, task_id);
    let stage_id = task
        .spec
        .as_ref()
        .and_then(|s| s.get("stage_class"))
        .and_then(|v| v.as_str())
        .unwrap_or(task_id)
        .to_string();

    let mut stderr = String::new();
    stderr.push_str(&reason);
    if !neutral_signal.is_empty() {
        stderr.push_str("\n\nDomain-correctness signal (method-neutral):\n");
        for s in &neutral_signal {
            stderr.push_str("- ");
            stderr.push_str(s);
            stderr.push('\n');
        }
    }

    // `captured_at` keys the proposer cache; derive it from the reason so
    // an unchanged block reuses the cache, and a new failure reason (the
    // reason carries the failed assertion ids) re-proposes.
    let captured_at = format!("validation:{}", short_digest(&reason));

    Some(ecaa_workflow_core::error_envelope::synthesize(
        ecaa_workflow_core::error_envelope::EnvelopeInput {
            task_id: ecaa_workflow_core::dag::TaskId::from(task_id),
            stage_id: stage_id.into(),
            library: None,
            library_version: None,
            stderr: stderr.as_str(),
            stdout: "",
            exit_code: None,
            signal: None,
            wallclock_secs: None,
            peak_memory_mb: None,
            input_summary: Default::default(),
            executor: "validation-contract".to_string(),
            executor_context: Default::default(),
            captured_at,
            attempt: read_overrides_attempts(package, task_id).saturating_add(1),
        },
    ))
}

/// Read the method-neutral statements from
/// `runtime/inputs/<task>/domain-correctness-signal.json` (the
/// harness-written autonomous-recovery signal). Returns an empty vec
/// when the file is absent / unreadable — the proposer still runs on the
/// blocked reason alone. Path-jailed on `task_id`.
fn read_domain_correctness_statements(package: &std::path::Path, task_id: &str) -> Vec<String> {
    let inputs_base = package.join("runtime/inputs");
    let Ok(task_inputs) = super::_path_jail::safe_segment_join(&inputs_base, task_id) else {
        return Vec::new();
    };
    let path = task_inputs.join("domain-correctness-signal.json");
    if super::_path_jail::assert_under_root(package, &path).is_err() {
        return Vec::new();
    }
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&raw) else {
        return Vec::new();
    };
    v.get("failed_assertions")
        .and_then(|a| a.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|e| e.get("statement").and_then(|s| s.as_str()))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// Tiny stable digest of a string for cache-keying. Deterministic; not
/// cryptographic. Uses the std hasher with a fixed seed (default-state
/// `DefaultHasher`), which is stable within a process run — sufficient
/// for the in-process proposer cache.
fn short_digest(s: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    s.hash(&mut h);
    format!("{:016x}", h.finish())
}

fn read_envelope(
    package: &std::path::Path,
    task_id: &str,
) -> anyhow::Result<Option<ToolErrorEnvelope>> {
    let p = match resolve_envelope_path(package, task_id) {
        Ok(p) => p,
        Err(_) => return Ok(None),
    };
    if !p.exists() {
        return Ok(None);
    }
    let raw = std::fs::read_to_string(&p)?;
    let env: ToolErrorEnvelope = serde_json::from_str(&raw)?;
    Ok(Some(env))
}

fn read_overrides(package: &std::path::Path, task_id: &str) -> Option<ExecutorOverrides> {
    let p = resolve_overrides_path(package, task_id).ok()?;
    if !p.exists() {
        return None;
    }
    let raw = std::fs::read_to_string(&p).ok()?;
    serde_json::from_str(&raw).ok()
}

fn read_overrides_attempts(package: &std::path::Path, task_id: &str) -> u32 {
    read_overrides(package, task_id)
        .map(|o| o.attempts_consumed)
        .unwrap_or(0)
}

fn write_overrides(
    package: &std::path::Path,
    task_id: &str,
    overrides: &ExecutorOverrides,
) -> anyhow::Result<()> {
    let path = resolve_overrides_path(package, task_id)
        .map_err(|s| anyhow::anyhow!("path jail rejected task_id: {:?}", s))?;
    let dir = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("resolved overrides path has no parent"))?;
    std::fs::create_dir_all(dir)?;
    // Belt-and-suspenders: after the parent exists, assert_under_root
    // would fail on a symlink planted between segment-check and write.
    super::_path_jail::assert_under_root(package, &path)
        .map_err(|e| anyhow::anyhow!("path escapes package root: {e}"))?;
    let raw = serde_json::to_string_pretty(overrides)?;
    ecaa_workflow_core::fs_helpers::atomic_write_bytes_sync(&path, raw.as_bytes())?;
    Ok(())
}

fn kind_label(k: &RemediationKind) -> &'static str {
    match k {
        RemediationKind::BumpResources { .. } => "resource bump",
        RemediationKind::SwitchMethod { .. } => "method swap",
        RemediationKind::PinLibraryVersion { .. } => "library pin",
        RemediationKind::OverrideParameter { .. } => "parameter override",
        RemediationKind::SwapInputData { .. } => "input swap",
        RemediationKind::RerunUpstream { .. } => "upstream rerun",
        RemediationKind::TweakExecutor { .. } => "executor tweak",
        RemediationKind::RetryAsIs { .. } => "retry-as-is",
        RemediationKind::RebuildEnvironment { .. } => "rebuild env",
        RemediationKind::ManualReview { .. } => "manual review",
    }
}

// Suppress unused-warning for ExecutionHandle re-export (keeps the
// import chain explicit alongside the other chat_routes modules even
// when no field is read here).
#[allow(dead_code)] // reserved-for-import-alive: anchors ExecutionHandle re-export
fn _eh_silencer(_: &ExecutionHandle) {}

/// Route inventory for the doc-as-contract gate +
/// per-submodule `routes()` builder. `mod.rs::router()` merges every
/// submodule's builder into the single chat surface.
pub(super) const ROUTES: &[(&str, &str)] = &[
    (
        "GET",
        "/api/chat/session/:id/task/:task_id/remediation-suggestions",
    ),
    (
        "POST",
        "/api/chat/session/:id/task/:task_id/apply-remediation",
    ),
];

pub(super) fn routes() -> axum::Router<ChatAppState> {
    axum::Router::new()
        .route(
            "/api/chat/session/:id/task/:task_id/remediation-suggestions",
            axum::routing::get(get_remediation_suggestions),
        )
        .route(
            "/api/chat/session/:id/task/:task_id/apply-remediation",
            axum::routing::post(post_apply_remediation),
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use ecaa_workflow_core::remediation::{
        RemediationKind, RemediationSuggestion, ResourceTarget, SuggestionConfidence, ToolBinding,
    };

    fn sugg() -> RemediationSuggestion {
        RemediationSuggestion {
            id: "rs-1".into(),
            kind: RemediationKind::BumpResources {
                target: ResourceTarget {
                    memory_gb: Some(64),
                    ..Default::default()
                },
                prior: None,
            },
            rationale: "OOM".into(),
            confidence: SuggestionConfidence::High,
            evidence: vec!["error_class".into()],
            tool_binding: ToolBinding::RerunTask,
            estimated_cost_delta_usd: None,
        }
    }

    #[test]
    fn read_overrides_returns_none_when_absent() {
        let tmp = tempfile::tempdir().unwrap();
        let pkg = tmp.path();
        assert!(read_overrides(pkg, "x").is_none());
        assert_eq!(read_overrides_attempts(pkg, "x"), 0);
    }

    /// Build a one-task DAG whose single task is in `state`.
    fn dag_with_task(task_id: &str, state: ecaa_workflow_core::dag::TaskState) -> ecaa_workflow_core::dag::DAG {
        use ecaa_workflow_core::dag::{Assignee, ResourceClass, Task, TaskId, TaskKind, DAG};
        let mut tasks: std::collections::BTreeMap<TaskId, Task> = std::collections::BTreeMap::new();
        tasks.insert(
            task_id.into(),
            Task {
                kind: TaskKind::Computation,
                state,
                depends_on: vec![],
                assignee: Assignee::Agent,
                description: "t".into(),
                spec: Some(serde_json::json!({"stage_class": task_id})),
                resolution: None,
                result_ref: None,
                resource_class: ResourceClass::CpuHeavy,
                requires_sme_review: false,
                required_artifacts: vec![],
                container: None,
                source_atom_id: None,
                safety: Default::default(),
                inputs: Vec::new(),
                outputs: Vec::new(),
                edam_operation: None,
                execution_index: None,
            },
        );
        DAG {
            version: "1.0".into(),
            schema_version: ecaa_workflow_core::dag::current_dag_schema_version(),
            workflow_id: "t".into(),
            current_task: None,
            tasks,
            reverse_deps: std::collections::BTreeMap::new(),
            run_id: None,
            execution_order: Vec::new(),
        }
    }

    fn blocked(reason: &str) -> ecaa_workflow_core::dag::TaskState {
        ecaa_workflow_core::dag::TaskState::Blocked {
            record: ecaa_workflow_core::dag::BlockedRecord {
                reason: reason.into(),
                attempts: vec![],
            },
        }
    }

    /// Trigger widening: a task Blocked on a validation-contract reason
    /// yields a synthesized envelope carrying the method-neutral signal.
    #[test]
    fn validation_block_synthesizes_envelope_with_neutral_signal() {
        let tmp = tempfile::tempdir().unwrap();
        let pkg = tmp.path();
        // Harness-written neutral signal.
        let sig_dir = pkg.join("runtime/inputs/variant_calling");
        std::fs::create_dir_all(&sig_dir).unwrap();
        std::fs::write(
            sig_dir.join("domain-correctness-signal.json"),
            r#"{"failed_assertions":[{"assertion_id":"variant_calling.het_tail_band_nonempty","statement":"variant_calling.het_tail_band_nonempty: this design requires /low_af_band_count at least 1, but your result.json recomputes 0 — revisit (no method prescribed)."}]}"#,
        )
        .unwrap();

        let mut session = ecaa_workflow_conversation::session::Session::new(false);
        session.dag = Some(dag_with_task(
            "variant_calling",
            blocked("Harness validation-contract check: required assertion(s) unsatisfied: variant_calling.het_tail_band_nonempty."),
        ));

        let env = synthesize_validation_failed_envelope(&session, pkg, "variant_calling")
            .expect("a validation-contract block must synthesize an envelope");
        assert_eq!(env.executor, "validation-contract");
        let joined = env.stderr_tail.join("\n");
        assert!(
            joined.contains("at least 1") && joined.contains("recomputes 0"),
            "envelope must carry the neutral recomputed-bound statement: {joined}"
        );
        // Neutrality: the synthesized stderr names no tool/flag.
        let lower = joined.to_ascii_lowercase();
        for token in ["lofreq", "gatk", "--", "set the threshold"] {
            assert!(!lower.contains(token), "leaked {token:?}: {joined}");
        }
    }

    /// A task Blocked on a NON-validation reason (or not blocked) is not
    /// eligible — the proposer trigger is not widened to every blocker.
    #[test]
    fn non_validation_block_returns_none() {
        let tmp = tempfile::tempdir().unwrap();
        let pkg = tmp.path();
        let mut session = ecaa_workflow_conversation::session::Session::new(false);
        session.dag = Some(dag_with_task(
            "alignment",
            blocked("Agent needs the SME to pick a reference genome build."),
        ));
        assert!(
            synthesize_validation_failed_envelope(&session, pkg, "alignment").is_none(),
            "a non-validation block must not synthesize an envelope"
        );

        // A completed task is likewise ineligible.
        session.dag = Some(dag_with_task(
            "alignment",
            ecaa_workflow_core::dag::TaskState::Completed {
                result: serde_json::json!({}),
            },
        ));
        assert!(synthesize_validation_failed_envelope(&session, pkg, "alignment").is_none());
    }

    #[test]
    fn write_then_read_overrides_roundtrips() {
        let tmp = tempfile::tempdir().unwrap();
        let pkg = tmp.path();
        let mut ov = ExecutorOverrides::default();
        let s = sugg();
        ov.merge(&s, "now".into(), "sme".into()).unwrap();
        write_overrides(pkg, "alignment", &ov).unwrap();
        let back = read_overrides(pkg, "alignment").unwrap();
        assert_eq!(back.attempts_consumed, 1);
        assert_eq!(back.history.len(), 1);
    }

    // Remediation read/write paths must reject traversal-bearing
    // task_ids.
    #[test]
    fn write_overrides_rejects_traversal() {
        let tmp = tempfile::tempdir().unwrap();
        let pkg = tmp.path();
        let mut ov = ExecutorOverrides::default();
        let s = sugg();
        ov.merge(&s, "now".into(), "sme".into()).unwrap();
        // Traversal task_id must be rejected by the jail.
        assert!(write_overrides(pkg, "../../tmp/pwn", &ov).is_err());
    }

    #[test]
    fn resolve_overrides_rejects_traversal() {
        let tmp = tempfile::tempdir().unwrap();
        let pkg = tmp.path();
        std::fs::create_dir_all(pkg.join("runtime/inputs/legit")).unwrap();
        let result = resolve_overrides_path(pkg, "../../tmp/pwn");
        assert!(result.is_err());
    }

    #[test]
    fn resolve_envelope_rejects_traversal() {
        let tmp = tempfile::tempdir().unwrap();
        let pkg = tmp.path();
        std::fs::create_dir_all(pkg.join("runtime/outputs/legit")).unwrap();
        let result = resolve_envelope_path(pkg, "../../etc");
        assert!(result.is_err());
    }

    #[test]
    fn read_envelope_returns_none_on_traversal() {
        let tmp = tempfile::tempdir().unwrap();
        let pkg = tmp.path();
        std::fs::create_dir_all(pkg.join("runtime/outputs/legit")).unwrap();
        // Path-jail rejection surfaces as Ok(None), matching the existing
        // "no envelope" semantics so callers respond with 404, not 500.
        let res = read_envelope(pkg, "../../etc").unwrap();
        assert!(res.is_none());
    }

    #[test]
    fn kind_label_covers_every_variant() {
        for k in [
            RemediationKind::BumpResources {
                target: Default::default(),
                prior: None,
            },
            RemediationKind::SwitchMethod {
                stage_id: "x".into(),
                from: "a".into(),
                to: "b".into(),
                switch_kind: ecaa_workflow_core::remediation::MethodSwitchKind::Algorithm,
            },
            RemediationKind::PinLibraryVersion {
                library: "x".into(),
                from: None,
                to: "1".into(),
            },
            RemediationKind::OverrideParameter {
                stage_id: "x".into(),
                param: "p".into(),
                from: None,
                to: serde_json::json!(1),
            },
            RemediationKind::SwapInputData {
                field: "f".into(),
                from: None,
                to: "x".into(),
                swap_kind: ecaa_workflow_core::remediation::InputSwapKind::Reference,
            },
            RemediationKind::RerunUpstream {
                producer_task_id: "p".into(),
                nested: Box::new(RemediationKind::RetryAsIs { reason: "x".into() }),
            },
            RemediationKind::TweakExecutor {
                disable_spot: true,
                partition: None,
                availability_zone: None,
            },
            RemediationKind::RetryAsIs { reason: "x".into() },
            RemediationKind::RebuildEnvironment {
                capability: "x".into(),
                operator_command_hint: "y".into(),
            },
            RemediationKind::ManualReview {
                summary: "x".into(),
                suggested_next_steps: vec![],
            },
        ] {
            assert!(!kind_label(&k).is_empty());
        }
    }
}
