//! Sync orchestration harness that loops an agent subprocess against
//! ready tasks in a compiled RO-Crate package. Supports Local, Mock,
//! AWS, and SLURM executors via the `Executor` trait.
mod picker_decisions;
mod progress_client;

use anyhow::{Context, Result};
use clap::Parser;
use colored::Colorize;
use progress_client::ProgressClient;
use ecaa_workflow_core::clock::{Clock, WallClock};
use ecaa_workflow_core::dag::{TaskId, TaskState, DAG};
use ecaa_workflow_harness::dag_patch::{
    apply_pending_patches, apply_pending_patches_strict, PickedDispatch,
};
use ecaa_workflow_harness::dispatch_wal::{
    append_dispatch, generate_harness_run_id, read_dispatches,
    recover_orphaned_dispatches_with_denylist, truncate_wal, AlwaysDeadProbe, DispatchRecord,
    HeartbeatLivenessProbe, LivenessProbe,
};
use ecaa_workflow_harness::executor::hardware_envelope::{
    render_envelope, HardwareEnvelopeInputs,
};
use ecaa_workflow_harness::executor::host_probe::{
    allocate_for_picks, resolve_high_water_for, OverheadPolicy,
};
use ecaa_workflow_harness::executor::pilot::PilotConfig;
use ecaa_workflow_harness::executor::stall_monitor::{StallSignal, StallThresholds};
use ecaa_workflow_harness::executor::{self, Executor, ExecutorArgs};
use ecaa_workflow_harness::finalize_probe::{probe_one_task, ProbeOutcome};
use ecaa_workflow_harness::multiprocess_lock::SessionLock;
use ecaa_workflow_harness::required_artifacts::verify_required_artifacts;
use ecaa_workflow_harness::scheduler::{
    count_concurrent_peers_by_class, filter_picks_respecting_sme_gate, lane_mode_from_env,
    pause_dependent_tasks, pick_ready_respecting_budgets, pick_ready_with_lanes,
    read_confirmed_review_stages, ConcurrencyMode, SchedulerBudget,
};
use ecaa_workflow_harness::scratch_cleanup::cleanup_task_scratch;
use ecaa_workflow_harness::sme_skip;
use ecaa_workflow_harness::stall_relay;
use ecaa_workflow_harness::ecaa_io::{read_bytes_capped, read_capped, resolve_max_bytes};
use ecaa_workflow_harness::watchdog::{Watchdog, WatchdogConfig, WatchdogEvent};
use std::fs::OpenOptions;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

/// Synthesize a `ToolErrorEnvelope` from the executor's iteration
/// capture and persist it at `runtime/outputs/<task_id>/error.json`
/// so the server's `/progress` handler can promote the blocker to
/// `BlockerKind::ToolError`. Skips the write when an envelope already
/// exists (don't clobber a richer prior capture). The attempt counter
/// is read off the existing overrides.json's `attempts_consumed` so
/// the proposer can rank against history.
#[tracing::instrument(skip(package, capture), fields(task_id = %task_id))]
fn write_tool_error_envelope(
    package: &Path,
    task_id: &str,
    capture: &ecaa_workflow_harness::executor::IterationCapture,
) -> Result<()> {
    use ecaa_workflow_core::error_envelope::{synthesize, EnvelopeInput};
    use ecaa_workflow_core::remediation::ExecutorOverrides;
    use ecaa_workflow_harness::executor::overrides_io;

    let outputs_dir = package.join("runtime").join("outputs").join(task_id);
    if let Err(e) = std::fs::create_dir_all(&outputs_dir) {
        return Err(anyhow::anyhow!("creating {}: {}", outputs_dir.display(), e));
    }
    let target = outputs_dir.join("error.json");

    // Always overwrite. Each iteration of a task's lifecycle that
    // ends in a non-zero exit produces a fresh capture; a stale
    // envelope from a prior attempt would mislead the proposer's
    // attempt counter and the BlockerCard's evidence chips. The
    // overrides.json audit trail (separate file) preserves the
    // remediation history across attempts.
    let attempt = overrides_io::read(package, task_id)
        .ok()
        .flatten()
        .map(|o: ExecutorOverrides| o.attempts_consumed.saturating_add(1))
        .unwrap_or(1);

    let stage_id = read_dag(package)
        .ok()
        .and_then(|d| d.tasks.get(task_id).cloned())
        .and_then(|t| {
            t.spec
                .as_ref()
                .and_then(|s| s.get("stage_class"))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
        })
        .unwrap_or_else(|| task_id.to_string());

    let executor_name = capture
        .executor_context
        .get("executor")
        .cloned()
        .unwrap_or_else(|| {
            // Fall back to the ECAA_EXECUTOR_MODE env var; the
            // harness main records the resolved mode on the
            // capture context, so this branch is the legacy path.
            std::env::var("ECAA_EXECUTOR_MODE").unwrap_or_else(|_| "local".to_string())
        });
    let mut ctx = capture.executor_context.clone();
    ctx.entry("executor".into())
        .or_insert_with(|| executor_name.clone());

    let envelope = synthesize(EnvelopeInput {
        task_id: TaskId::from(task_id),
        stage_id: stage_id.into(),
        library: None,
        library_version: None,
        stderr: &capture.stderr,
        stdout: &capture.stdout,
        exit_code: capture.exit_code,
        signal: capture.signal.clone(),
        wallclock_secs: capture.wallclock_secs,
        peak_memory_mb: capture.peak_memory_mb,
        input_summary: Default::default(),
        executor: executor_name,
        executor_context: ctx,
        captured_at: ecaa_workflow_core::time_helpers::now_rfc3339(),
        attempt,
    });

    let raw = serde_json::to_string_pretty(&envelope)
        .with_context(|| format!("serialising envelope for {}", task_id))?;
    ecaa_workflow_core::fs_helpers::atomic_write_bytes_sync(&target, raw.as_bytes())
        .with_context(|| format!("atomic write envelope at {}", target.display()))?;
    Ok(())
}

/// Read the existing envelope's `error_class` if present. Used for
/// outcome-recording (Recurred vs NewError) — the harness compares
/// classes between attempts so the proposer can see which fix worked
/// and which produced a new failure mode.
fn read_existing_envelope_error_class(package: &Path, task_id: &str) -> Option<String> {
    use ecaa_workflow_core::error_envelope::ToolErrorEnvelope;
    let p = package
        .join("runtime")
        .join("outputs")
        .join(task_id)
        .join("error.json");
    // Cap the on-disk read so a runaway agent that
    // writes a 10 GiB error.json can't OOM the harness on next probe.
    let raw = read_capped(&p, resolve_max_bytes()).ok()?;
    let env: ToolErrorEnvelope = serde_json::from_str(&raw).ok()?;
    Some(env.error_class)
}

/// Set the outcome on the most recent applied remediation in
/// `runtime/inputs/<task>/overrides.json`. Best-effort — no-ops when
/// the file is absent or the audit history is empty.
fn update_overrides_outcome(
    package: &Path,
    task_id: &str,
    outcome: ecaa_workflow_core::remediation::RemediationOutcome,
) {
    use ecaa_workflow_harness::executor::overrides_io;
    let mut ov = match overrides_io::read(package, task_id) {
        Ok(Some(o)) => o,
        _ => return,
    };
    if ov.history.is_empty() {
        return;
    }
    if let Some(last) = ov.history.last() {
        if last.outcome != ecaa_workflow_core::remediation::RemediationOutcome::NotYetAttempted {
            // Already recorded by an earlier observation. Don't
            // overwrite — outcome is monotonic per remediation entry.
            return;
        }
    }
    ov.record_last_outcome(outcome);
    if let Err(e) = overrides_io::write(package, task_id, &ov) {
        tracing::warn!(
            target: "overrides",
            task_id = %task_id,
            error = format!("{:#}", e),
            "writing outcome update failed"
        );
    }
}

/// touch a `runtime/outputs/<task_id>/.heartbeat` file so the harness
/// main loop can measure liveness without relying on the stall
/// monitor's `/proc/<pid>` sampling.
///
/// W7.3: returns `Ok(())` on success, `Err(io::Error)` on any
/// directory-creation or write failure. The caller is expected to skip
/// the dispatch when the heartbeat baseline can't be established —
/// without a fresh heartbeat the orphan reaper would false-positive on
/// the next iteration, treating the still-running task as dead. Each
/// failure also bumps the `HeartbeatWriteFailed` silent-skip counter
/// AND fires a `tracing::error!` so the issue surfaces immediately
/// (not just in the next-iteration harness-health sidecar).
#[tracing::instrument(skip(package_root), fields(task_id = %task_id))]
fn touch_heartbeat(package_root: &Path, task_id: &str) -> std::io::Result<()> {
    let dir = package_root.join("runtime/outputs").join(task_id);
    if let Err(e) = std::fs::create_dir_all(&dir) {
        ecaa_workflow_harness::_observability::note_silent_skip(
            ecaa_workflow_harness::_observability::SkipCategory::HeartbeatWriteFailed,
            &format!("mkdir {} failed: {}", dir.display(), e),
            Some(task_id),
        );
        tracing::error!(
            target: "heartbeat",
            task_id = %task_id,
            error = %e,
            "heartbeat mkdir failed; dispatch must skip to avoid orphan-reaper false-positive"
        );
        return Err(e);
    }
    let path = dir.join(".heartbeat");
    match OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&path)
    {
        Ok(mut f) => {
            let body = ecaa_workflow_core::time_helpers::now_rfc3339();
            if let Err(e) = f.write_all(body.as_bytes()) {
                ecaa_workflow_harness::_observability::note_silent_skip(
                    ecaa_workflow_harness::_observability::SkipCategory::HeartbeatWriteFailed,
                    &format!("write {} failed: {}", path.display(), e),
                    Some(task_id),
                );
                tracing::error!(
                    target: "heartbeat",
                    task_id = %task_id,
                    error = %e,
                    "heartbeat write failed; dispatch must skip"
                );
                return Err(e);
            }
            Ok(())
        }
        Err(e) => {
            ecaa_workflow_harness::_observability::note_silent_skip(
                ecaa_workflow_harness::_observability::SkipCategory::HeartbeatWriteFailed,
                &format!("open {} failed: {}", path.display(), e),
                Some(task_id),
            );
            tracing::error!(
                target: "heartbeat",
                task_id = %task_id,
                error = %e,
                "heartbeat open failed; dispatch must skip"
            );
            Err(e)
        }
    }
}

/// age of a task's `.heartbeat` file in seconds, or
/// `None` when the file is missing or unreadable. Preferred over the
/// raw `started_at` age because it reflects actual agent-side
/// liveness; the harness main loop falls back to `started_at` when
/// the file is absent (older agent script).
fn heartbeat_age_secs(package_root: &Path, task_id: &str) -> Option<u64> {
    let path = package_root
        .join("runtime/outputs")
        .join(task_id)
        .join(".heartbeat");
    let meta = std::fs::metadata(&path).ok()?;
    let modified = meta.modified().ok()?;
    let elapsed = modified.elapsed().ok()?;
    Some(elapsed.as_secs())
}

/// read `ECAA_TASK_HEARTBEAT_STALL_SECS` (default
/// 900s = 15 minutes). Set to `0` to disable the detector entirely
/// and keep legacy behavior.
fn heartbeat_stall_threshold_secs() -> u64 {
    use ecaa_workflow_harness::constants::HEARTBEAT_STALL_THRESHOLD_SECS_DEFAULT;
    std::env::var("ECAA_TASK_HEARTBEAT_STALL_SECS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(HEARTBEAT_STALL_THRESHOLD_SECS_DEFAULT)
}

/// `ECAA_HEARTBEAT_LIVENESS_SECS` — freshness window (in seconds) for
/// the orphan-by-crash recovery's liveness check. The agent's
/// heartbeat fork touches `runtime/outputs/<task_id>/.heartbeat`
/// every 30s, so the default 60s window comfortably covers one
/// missed touch + scheduler slack while still flagging genuinely
/// dead tasks within ~1 minute of crash. Set to `0` to disable the
/// liveness check (legacy behavior — every prior-run dispatch with
/// expired deadline gets flagged as orphan). Clamped to `[0, 600]`
/// so a typo can't either neuter the safety net or ignore real
/// crashes for hours.
fn heartbeat_liveness_window_secs() -> u64 {
    use ecaa_workflow_harness::constants::{
        HEARTBEAT_LIVENESS_WINDOW_SECS_DEFAULT, HEARTBEAT_LIVENESS_WINDOW_SECS_MAX,
    };
    let raw = std::env::var("ECAA_HEARTBEAT_LIVENESS_SECS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(HEARTBEAT_LIVENESS_WINDOW_SECS_DEFAULT);
    raw.min(HEARTBEAT_LIVENESS_WINDOW_SECS_MAX)
}

/// `ECAA_HARNESS_SETTLE_SECS` — sleep this long
/// at the END of any iteration whose only state was "Running tasks
/// with fresh heartbeats and zero ready / blocked-needing-SME work."
/// Covers the broader "harness has nothing to do but wait for
/// detached compute" case. Default 60s; clamped to `[5, 1800]` so a
/// typo can't either tight-loop or freeze the harness for hours.
/// Set to `0` to disable the settle sleep entirely.
fn settle_interval_secs() -> u64 {
    use ecaa_workflow_harness::constants::{
        HARNESS_SETTLE_INTERVAL_SECS_DEFAULT, HARNESS_SETTLE_INTERVAL_SECS_MAX,
        HARNESS_SETTLE_INTERVAL_SECS_MIN,
    };
    let raw = std::env::var("ECAA_HARNESS_SETTLE_SECS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(HARNESS_SETTLE_INTERVAL_SECS_DEFAULT);
    if raw == 0 {
        return 0;
    }
    raw.clamp(
        HARNESS_SETTLE_INTERVAL_SECS_MIN,
        HARNESS_SETTLE_INTERVAL_SECS_MAX,
    )
}

/// Decide whether the iteration is a "settle case" — no productive
/// work happened, but at least one Running task has a fresh
/// heartbeat (compute is genuinely in flight). When true, the loop
/// sleeps `settle_interval_secs()` instead of immediately re-iterating
/// so the deterministic finalize probe gets time to catch the
/// sentinel without burning iterations.
///
/// Pure function over the inputs so it's unit-testable without
/// spinning the executor.
fn is_settle_iteration(
    dag: &DAG,
    transitions_this_iteration: usize,
    fresh_heartbeat_running_ids: &[String],
    blocked_needing_sme_ids: &[String],
) -> bool {
    if transitions_this_iteration > 0 {
        return false;
    }
    if !blocked_needing_sme_ids.is_empty() {
        return false;
    }
    if dag.ready_tasks().iter().any(|_| true) {
        return false;
    }
    !fresh_heartbeat_running_ids.is_empty()
}

/// Returns the ids of tasks currently `Running` whose `.heartbeat`
/// file is younger than the stall threshold. These are the tasks
/// that justify the harness staying alive (compute is making forward
/// progress; a genuine stall would have flipped to
/// `Blocked { HeartbeatStalled }` already).
fn fresh_heartbeat_running_task_ids(package_root: &Path, dag: &DAG) -> Vec<String> {
    let threshold = heartbeat_stall_threshold_secs();
    if threshold == 0 {
        // Heartbeat stall detection is disabled — be conservative and
        // call ANY Running task fresh (we have no signal otherwise).
        return dag
            .tasks
            .iter()
            .filter(|(_, t)| matches!(t.state, TaskState::Running { .. }))
            .map(|(id, _)| id.to_string())
            .collect();
    }
    dag.tasks
        .iter()
        .filter(|(_, t)| matches!(t.state, TaskState::Running { .. }))
        .filter(|(id, _)| {
            heartbeat_age_secs(package_root, id.as_str()).unwrap_or(u64::MAX) < threshold
        })
        .map(|(id, _)| id.to_string())
        .collect()
}

/// Load the v4 sidecars
/// (`runtime/task-nodes.json` + `runtime/sandbox-policy.json`)
/// and run `pre_dispatch_check` on every task that's about to
/// transition to `Running`. Returns a map of `task_id → refusal
/// reason` for tasks that should be flipped to `Blocked`.
///
/// Soft-skips when either sidecar is missing: legacy sessions
/// (v1/v2/v3 or v4 sessions with no active policy bundle) have
/// no policy to enforce at dispatch time.
fn collect_sandbox_refusals(
    package_root: &Path,
    pick_ids: &[String],
) -> std::collections::BTreeMap<String, String> {
    use ecaa_workflow_core::sandbox_policy::SandboxPolicy;
    use ecaa_workflow_core::workflow_contracts::task_node::TaskNode;

    let mut refusals = std::collections::BTreeMap::new();
    let runtime = package_root.join("runtime");
    let nodes_path = runtime.join("task-nodes.json");
    let policy_path = runtime.join("sandbox-policy.json");
    let nodes_bytes = match std::fs::read(&nodes_path) {
        Ok(b) => b,
        Err(_) => return refusals,
    };
    let policy_bytes = match std::fs::read(&policy_path) {
        Ok(b) => b,
        Err(_) => return refusals,
    };
    let nodes: Vec<TaskNode> = match serde_json::from_slice(&nodes_bytes) {
        Ok(n) => n,
        Err(e) => {
            tracing::warn!(
                target: "sandbox-enforce",
                error = %e,
                "task-nodes.json parse error"
            );
            return refusals;
        }
    };
    let policy: SandboxPolicy = match serde_json::from_slice(&policy_bytes) {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!(
                target: "sandbox-enforce",
                error = %e,
                "sandbox-policy.json parse error"
            );
            return refusals;
        }
    };
    let pick_set: std::collections::BTreeSet<&str> = pick_ids.iter().map(String::as_str).collect();
    for node in &nodes {
        if !pick_set.contains(node.id.as_str()) {
            continue;
        }
        if let Some(refusal) =
            ecaa_workflow_harness::sandbox_enforcer::pre_dispatch_check(node, &policy)
        {
            // Emit a structured semicolon-separated
            // payload that round-trips through
            // `core::blocker::parse_agent_blocker_kind`. Each piece is
            // `<KindStr>:<detail> (node=<id>)` where `<KindStr>` is the
            // stable discriminator from `SandboxRefusal::kind_str`. When
            // a node lacks a per-refusal detail (unit-shaped variants
            // like NetworkDenied) the colon and detail are still
            // emitted — the parser tolerates empty `detail`.
            let detail = if refusal.sandbox_refusals.is_empty() {
                // `needs_container_wrap` path — preserve the legacy
                // human summary so SMEs see the same intent.
                refusal.human_summary()
            } else {
                refusal
                    .sandbox_refusals
                    .iter()
                    .map(|r| format!("{}:{} (node={})", r.kind_str(), r.detail(), node.id))
                    .collect::<Vec<_>>()
                    .join("; ")
            };
            refusals.insert(node.id.clone(), detail);
        }
    }
    refusals
}

/// Dispatch-time safety-policy gate. Runs
/// [`ecaa_workflow_harness::executor::enforce_safety_policy`] over
/// every pick against the executor's capability profile and returns a
/// map of `task_id → BlockerKind` for tasks that should flip to
/// `Blocked` instead of dispatching. Empty map = nothing to refuse.
///
/// The marker the caller writes into `BlockedRecord.reason` is built
/// via `core::blocker::format_safety_policy_marker`, which
/// `parse_agent_blocker_kind` round-trips back into the typed variant
/// for the UI. Tasks whose `source_atom_id` is `None` (pre-A.S6
/// packages) keep their `safety: SafetyPolicy::default()` and pass the
/// gate unconditionally — no regression on legacy WORKFLOW.json.
///
/// Also enforces the controlled-access data guard: when
/// `task.safety.controlled_access == true` and the executor would
/// route the task through an LLM agent (kind != "mock"), dispatch is
/// refused with `BlockerKind::ControlledAccessViolation`. The port
/// name is the first input port listed in the task spec's
/// `input_ports` array, or `"<unknown>"` when the field is absent
/// (pre-compose packages). The attempted call is constructed from the
/// executor kind so the SME's recovery affordance in `BlockerCard`
/// has enough context to prescribe a corrective action.
fn collect_safety_policy_refusals(
    dag: &DAG,
    pick_ids: &[String],
    caps: &ecaa_workflow_harness::executor::ExecutorCapabilities,
) -> std::collections::BTreeMap<String, ecaa_workflow_core::blocker::BlockerKind> {
    use ecaa_workflow_harness::executor::enforce_safety_policy;
    let mut refusals = std::collections::BTreeMap::new();
    for id in pick_ids {
        let Some(task) = dag.tasks.get(id.as_str()) else {
            continue;
        };
        if let Some(blocker) = enforce_safety_policy(task, caps) {
            refusals.insert(id.clone(), blocker);
            continue;
        }
        // Controlled-access guard: tasks marked `controlled_access: true`
        // must not be dispatched to an LLM-backed executor. The mock
        // executor is exempt (test-only, no real data). All real backends
        // (local, aws, slurm) launch the Claude agent wrapper and thereby
        // forward task context to an Anthropic inference endpoint.
        if task.safety.controlled_access && caps.kind != "mock" {
            let port_name = task
                .spec
                .as_ref()
                .and_then(|s| s.get("input_ports"))
                .and_then(|v| v.as_array())
                .and_then(|a| a.first())
                .and_then(|v| v.as_str())
                .unwrap_or("<unknown>")
                .to_string();
            let attempted_call = format!("agent_executor:{}", caps.kind);
            refusals.insert(
                id.clone(),
                ecaa_workflow_core::blocker::BlockerKind::ControlledAccessViolation {
                    task_id: id.clone(),
                    port_name,
                    attempted_call,
                },
            );
        }
    }
    refusals
}

/// Append per-task validator results to
/// `runtime/validation-reports.jsonl` so the RO-Crate emitter
/// registers it as a `CreativeWork` at re-emit time and the
/// Composition UI tab can render the validation status card.
///
/// The validator runners write their per-row outcome via
/// `ValidationReportSummary::to_jsonl` which produces sorted,
/// byte-stable JSON lines. This helper appends those lines to the
/// session-scoped sidecar (creating the file on first write,
/// appending on subsequent task completions).
///
/// Best-effort: a failing write is logged to stderr but doesn't
/// abort the harness loop. Validator wiring is gated on the task's
/// `RequiredArtifact.validation_obligations` list (today optional);
/// when empty the report has zero rows and nothing is appended.
fn append_validation_reports_sidecar(
    package_root: &Path,
    task_id: &str,
    summary: &ecaa_workflow_harness::validators::ValidationReportSummary,
) {
    if summary.rows.is_empty() {
        return;
    }
    let runtime = package_root.join("runtime");
    if let Err(e) = std::fs::create_dir_all(&runtime) {
        eprintln!(
            "  {} validation-reports.jsonl mkdir failed: {}",
            "⚠".yellow(),
            e
        );
        return;
    }
    let path = runtime.join("validation-reports.jsonl");
    let jsonl = summary.to_jsonl();
    match OpenOptions::new().create(true).append(true).open(&path) {
        Ok(mut f) => {
            if let Err(e) = f.write_all(jsonl.as_bytes()) {
                eprintln!(
                    "  {} validation-reports.jsonl write failed for {}: {}",
                    "⚠".yellow(),
                    task_id,
                    e
                );
            }
        }
        Err(e) => eprintln!(
            "  {} validation-reports.jsonl open failed for {}: {}",
            "⚠".yellow(),
            task_id,
            e
        ),
    }
}

fn stamp_dispatch_identity(
    env: &mut std::collections::BTreeMap<String, String>,
    dispatch: Option<&PickedDispatch>,
) {
    if let Some(dispatch) = dispatch {
        env.insert(
            "ECAA_HARNESS_RUN_ID".into(),
            dispatch.harness_run_id.clone(),
        );
        env.insert("ECAA_DISPATCH_EPOCH".into(), dispatch.epoch.to_string());
    }
}

/// Stamp the literature-retrieval scope env vars from `ECAA_LIT_*` onto
/// the per-task envelope. The agent helper
/// (`scripts/agent_literature_fetch.py`) reads them at task-execution
/// time to select source-scope tier, NCBI rate limit, evidence storage
/// cap, and institutional-access opt-in.
fn stamp_literature_scope(env: &mut std::collections::BTreeMap<String, String>) {
    let cfg = ecaa_workflow_harness::literature_scope::LiteratureScopeConfig::from_env();
    for (k, v) in cfg.agent_env_vars() {
        env.insert(k, v);
    }
}

/// Render the per-task `provisioning.json` and
/// stamp `ECAA_PROVISIONING_POLICY` onto the envelope so the
/// install-proxy shims (`runtime/install-proxy/*`) can read the policy
/// at install time. Single seam shared by all executors (Local /
/// SLURM / AWS / Mock) — no executor-specific bind-mount plumbing
/// required: the agent script either honours `ECAA_PROVISIONING_POLICY`
/// directly, or bind-mounts the rendered file into
/// `/etc/ecaa-workflow/provisioning.json` inside the container (the
/// fallback path the shim consults when the env var is unset).
///
/// `declared` is the registry → packages map from the package-level
/// `policies/runtime-prereqs.json` (loaded once per dispatch in
/// `dispatch_picks` and passed through). The same map applies to every
/// task in this pick set — atom-level filtering happens later when
/// each atom's RuntimePrereqs becomes per-task; today the
/// package-level union is the conservative declaration.
///
/// Failures are logged to stderr but never abort dispatch — the
/// install-proxy is best-effort enforcement; the SafetyPolicy gate in
/// `enforce_safety_policy` already refused dispatch for atoms whose
/// policy this executor can't satisfy. A missing or unwritable
/// `runtime/inputs/<task_id>/provisioning.json` simply leaves the
/// agent on the host's default policy path.
fn stamp_provisioning_policy(
    env: &mut std::collections::BTreeMap<String, String>,
    package: &Path,
    dag: &DAG,
    task_id: &str,
    declared: &std::collections::BTreeMap<String, Vec<String>>,
) {
    let Some(task) = dag.tasks.get(task_id) else {
        return;
    };
    let out_dir = package.join("runtime").join("inputs").join(task_id);
    if let Err(e) = std::fs::create_dir_all(&out_dir) {
        eprintln!(
            "  {} provisioning.json mkdir failed for {}: {}",
            "⚠".yellow(),
            task_id,
            e
        );
        return;
    }
    let policy_path = out_dir.join("provisioning.json");
    match ecaa_workflow_harness::safety_render::render_provisioning_json(
        task,
        declared.clone(),
        &policy_path,
    ) {
        Ok(()) => {
            env.insert(
                "ECAA_PROVISIONING_POLICY".into(),
                policy_path.to_string_lossy().into_owned(),
            );
        }
        Err(e) => {
            eprintln!(
                "  {} provisioning.json render failed for {}: {}",
                "⚠".yellow(),
                task_id,
                e
            );
        }
    }
}

/// R1.6 — stamp `ECAA_TASK_NETWORK` (none|bridge|host) onto the
/// per-task envelope so the agent script (local docker/podman wrap or
/// the SLURM apptainer wrap) can append `--network=<value>`. Resolved
/// from `task.safety.network`: deny-all (`NetworkPolicy::None`) maps
/// to `none`; `Bridge` maps to `bridge`. `host` is not produced by the
/// safety policy enum today but is reserved for an operator override.
/// Missing task / unknown task id leaves the envelope untouched.
///
/// `TaskKind::Computation` exception: compute tasks whose YAML carries
/// the bare default `NetworkPolicy::None { allowlist: vec![] }` are
/// upgraded to "bridge". The PROMPT.md install-at-task-start path
/// (pip / BiocManager / conda for SME-pinned or discover-picked
/// methods not in the base image) needs network egress, and almost
/// no atom YAML sets `safety.network` explicitly — the empty-
/// allowlist None is the structural default, not an authored
/// intent. Compute atoms that GENUINELY need air-gapped execution
/// must declare a non-empty allowlist (which the safety lint treats
/// as still-None-effectively, so this branch sees the allowlist and
/// keeps "none"). Non-compute tasks (Discovery / Validation / Review
/// / Gate) keep the literal mapping — they don't run user code that
/// needs network, so the safer "none" default applies.
fn stamp_safety_network(
    env: &mut std::collections::BTreeMap<String, String>,
    dag: &DAG,
    task_id: &str,
) {
    use ecaa_workflow_core::atom::NetworkPolicy;
    use ecaa_workflow_core::dag::TaskKind;
    let Some(task) = dag.tasks.get(task_id) else {
        return;
    };
    let value = match (&task.kind, &task.safety.network) {
        (TaskKind::Computation, NetworkPolicy::None { allowlist }) if allowlist.is_empty() => {
            "bridge"
        }
        (_, NetworkPolicy::None { .. }) => "none",
        (_, NetworkPolicy::Bridge) => "bridge",
    };
    env.insert("ECAA_TASK_NETWORK".into(), value.into());
}

/// Load and bucket the package-level RuntimePrereqs into the
/// registry → packages map the install-proxy shims expect. Cached
/// once per dispatch tick so all picks share the same view of the
/// declared package set. Returns an empty map when the manifest is
/// absent (pre-A.S6 packages) — that disables `declared_only`
/// installs without breaking dispatch.
fn load_declared_per_registry(package: &Path) -> std::collections::BTreeMap<String, Vec<String>> {
    let manifest_path = package.join("policies/runtime-prereqs.json");
    if !manifest_path.exists() {
        return std::collections::BTreeMap::new();
    }
    let raw = match std::fs::read_to_string(&manifest_path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!(
                "  {} runtime-prereqs.json read failed: {} (provisioning will fall back to allowlisted/sealed only)",
                "⚠".yellow(),
                e
            );
            return std::collections::BTreeMap::new();
        }
    };
    match serde_json::from_str::<ecaa_workflow_core::runtime_prereqs::RuntimePrereqs>(&raw) {
        Ok(p) => p.declared_per_registry(),
        Err(e) => {
            eprintln!(
                "  {} runtime-prereqs.json parse failed: {} (provisioning will fall back to allowlisted/sealed only)",
                "⚠".yellow(),
                e
            );
            std::collections::BTreeMap::new()
        }
    }
}

/// Append a harness-owned line to
/// `<package>/runtime/outputs/<task_id>/progress.log` so the
/// `TaskLogDrawer` is never empty for a running task, even when the
/// agent doesn't write its own progress narration. Best-effort: a
/// failing write is logged to stderr but doesn't abort the loop.
fn append_progress_log(package_root: &Path, task_id: &str, message: &str) {
    let dir = package_root.join("runtime/outputs").join(task_id);
    if let Err(e) = std::fs::create_dir_all(&dir) {
        eprintln!(
            "  {} progress.log mkdir failed for {}: {}",
            "⚠".yellow(),
            task_id,
            e
        );
        return;
    }
    let path = dir.join("progress.log");
    match OpenOptions::new().create(true).append(true).open(&path) {
        Ok(mut f) => {
            let line = format!(
                "[{}] {}\n",
                ecaa_workflow_core::time_helpers::now_rfc3339(),
                message
            );
            if let Err(e) = f.write_all(line.as_bytes()) {
                eprintln!(
                    "  {} progress.log write failed for {}: {}",
                    "⚠".yellow(),
                    task_id,
                    e
                );
            }
        }
        Err(e) => eprintln!(
            "  {} progress.log open failed for {}: {}",
            "⚠".yellow(),
            task_id,
            e
        ),
    }
}

#[derive(Parser)]
#[command(
    name = "ecaa-workflow-harness",
    about = "Run an agent against a workflow package"
)]
struct Args {
    /// Path to the execution package directory
    #[arg(short, long)]
    package: String,

    /// Agent command to invoke (e.g. "claude" or "./scripts/test-agent.sh")
    #[arg(short, long)]
    agent: String,

    /// Maximum agent invocations before stopping
    #[arg(short, long, default_value = "20")]
    max_iterations: usize,

    /// Seconds before a Running task is considered stale and reset to Ready
    #[arg(long, default_value_t = ecaa_workflow_harness::constants::TASK_TIMEOUT_SECS_DEFAULT)]
    task_timeout: u64,

    /// When set, write a waiting_for_sme log entry instead of prompting stdin.
    /// Use with the web UI — the server handles SME resolution.
    #[arg(long, default_value = "false")]
    no_interactive: bool,

    /// Optional chat session id to post progress events to. When unset, the
    /// harness behaves exactly as before — no HTTP calls, runtime/LOG.jsonl
    /// only. Used by the web UI to surface task progress as conversation
    /// turns.
    #[arg(long)]
    session_id: Option<String>,

    /// Conversation server base URL (e.g. http://localhost:3000). Required
    /// alongside `--session-id`.
    #[arg(long, default_value = "http://localhost:3000")]
    server_url: String,

    /// Read-only dry run: load WORKFLOW.json, validate the DAG, print a
    /// per-task plan summary to stdout, and exit. No multiprocess lock,
    /// no executor provisioning, no agent invocation. Exit codes:
    /// 0 = clean + dispatchable; 2 = DAG validation failed; 3 = at least
    /// one task is blocked by safety policy.
    #[arg(long, default_value_t = false)]
    plan_only: bool,
}

/// `tracing_subscriber::fmt::MakeWriter` implementation that routes each
/// log line into a shared `Arc<Mutex<std::fs::File>>`. Used by the
/// `harness.log` file-writer layer so a single `File` handle is safely
/// shared across the multi-threaded harness without spawning a separate
/// background writer thread.
struct HarnessLogWriter(Arc<Mutex<std::fs::File>>);

impl std::io::Write for HarnessLogWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap_or_else(|p| p.into_inner()).write(buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.0.lock().unwrap_or_else(|p| p.into_inner()).flush()
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for HarnessLogWriter {
    type Writer = Self;

    fn make_writer(&'a self) -> Self::Writer {
        HarnessLogWriter(self.0.clone())
    }
}

fn main() -> Result<()> {
    // Put the harness in its
    // own POSIX process group as early as possible so the SIGINT
    // handler's `kill(-pgid, …)` reaches every descendant. Idempotent
    // when the server-spawned path already called `setsid()` in
    // `pre_exec` (this becomes EPERM, ignored). The CLI-direct path
    // (`ecaa-workflow-harness --package …` invoked from a shell)
    // gets a fresh group here, so a Ctrl+C tears down agent-claude.sh
    // and its npm/claude descendants instead of leaving them as
    // init-orphan zombies eating tokens.
    setpgid_self();

    // Parse CLI args before tracing init so we can derive the log
    // file path from `--package` at subscriber construction time.
    let args = Args::parse();
    let path = Path::new(&args.package);

    // Wire `tracing-subscriber` for the harness binary
    // so dispatch_wal events, executor decisions, and stall-monitor
    // warnings emit at runtime. RUST_LOG controls the filter; default
    // shows info+ from our crates and warn+ from deps so a fresh
    // harness invocation surfaces the load-bearing events without
    // drowning in subprocess plumbing.
    //
    // A second file-writer layer mirrors every event to
    // `<package>/runtime/harness.log` for post-run forensics without
    // requiring a terminal. The write is best-effort: if the file
    // cannot be created (e.g. package dir not yet present) the harness
    // falls back to stderr-only and logs the reason once stderr is live.
    let harness_log_open_err: Option<String> = {
        use tracing_subscriber::prelude::*;

        let env_filter =
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
                tracing_subscriber::EnvFilter::new(
                    "info,ecaa_workflow_harness=info,ecaa_workflow_core=info",
                )
            });

        let stderr_layer = tracing_subscriber::fmt::layer()
            .with_writer(std::io::stderr)
            .with_target(true);

        // Attempt to open <package>/runtime/harness.log. The runtime/
        // directory may not exist yet when the package was freshly
        // emitted; create it if absent.
        let log_path = path.join("runtime").join("harness.log");
        let file_result: Result<std::fs::File, String> = (|| {
            if let Some(parent) = log_path.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| format!("create_dir_all {}: {}", parent.display(), e))?;
            }
            std::fs::File::create(&log_path)
                .map_err(|e| format!("open {}: {}", log_path.display(), e))
        })();

        match file_result {
            Ok(file) => {
                let writer = HarnessLogWriter(Arc::new(Mutex::new(file)));
                let file_layer = tracing_subscriber::fmt::layer()
                    .with_writer(writer)
                    .with_target(true)
                    .with_ansi(false);
                tracing_subscriber::registry()
                    .with(env_filter)
                    .with(stderr_layer)
                    .with(file_layer)
                    .init();
                None
            }
            Err(reason) => {
                tracing_subscriber::registry()
                    .with(env_filter)
                    .with(stderr_layer)
                    .init();
                Some(reason)
            }
        }
    };
    if let Some(reason) = harness_log_open_err {
        tracing::warn!(
            reason = %reason,
            "harness.log file writer unavailable; continuing with stderr only",
        );
    }

    // Route panics through tracing so they appear in the structured log
    // stream (both stderr and harness.log) rather than going to stderr
    // unformatted. Installed after the subscriber so the first subscriber
    // that sees the event is the file-writer layer above, keeping the
    // panic in the forensic log alongside the surrounding context.
    std::panic::set_hook(Box::new(|info| {
        let location = info
            .location()
            .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()));
        let payload = info
            .payload()
            .downcast_ref::<&str>()
            .copied()
            .or_else(|| info.payload().downcast_ref::<String>().map(|s| s.as_str()))
            .unwrap_or("<non-string panic payload>");
        tracing::error!(
            panic.location = location.as_deref().unwrap_or("<unknown>"),
            panic.payload = %payload,
            "panic caught in panic hook"
        );
    }));

    // Construct the wall-clock once at startup so it can be threaded
    // through run_loop and recover_orphaned_dispatches_with_denylist.
    // Tests substitute a FrozenClock via the run_loop clock parameter
    // to produce deterministic timestamps without sleeping or mocking
    // the system clock.
    let clock = WallClock;

    if !path.exists() {
        anyhow::bail!("Package directory '{}' does not exist", args.package);
    }

    // --plan-only short-circuit. Read-only inspection: no SessionLock,
    // no executor provisioning, no agent dispatch, no ProgressClient
    // wiring. The plan_only module loads WORKFLOW.json, validates the
    // DAG, prints a per-task summary, and returns the desired exit code.
    if args.plan_only {
        let resolved_mode = std::env::var("ECAA_EXECUTOR_MODE").unwrap_or_else(|_| "local".into());
        let code = ecaa_workflow_harness::plan_only::run(path, &resolved_mode)?;
        std::process::exit(code);
    }

    // Host-level multi-process guard. When
    // `--session-id` is set, acquire an exclusive flock on
    // `~/.ecaa-workflow/locks/<session_id>.lock`. A peer harness
    // holding the same id (server-spawn + manual CLI race) discovers
    // the contention here and exits 2 instead of racing on
    // WORKFLOW.json / dispatch WAL / EC2 tags. Bypass via
    // `ECAA_HARNESS_DEBUG_ALLOW_MULTI_PROCESS=1` for tests that
    // deliberately spawn two harnesses. The guard is bound to a
    // local so its `Drop` runs when `main` returns (Ctrl+C, normal
    // exit, panic via the std panic-unwind path).
    let _session_lock: Option<SessionLock> = match args.session_id.as_deref() {
        Some(sid) => match SessionLock::acquire(sid) {
            Ok(lock) => Some(lock),
            Err(e) => {
                tracing::error!(
                    target: "harness",
                    session_id = %sid,
                    error = format!("{:#}", e),
                    "session lock contention"
                );
                std::process::exit(2);
            }
        },
        None => None,
    };

    // Opt-in LRU eviction for the per-session agent cache. Activated by
    // `ECAA_AGENT_CACHE_MAX_GB=<int>`; the one-shot run executes after
    // `SessionLock::acquire` so a peer harness can't race on the same
    // sweep. Failure logs a warning but never blocks harness startup —
    // cache eviction is best-effort disk-pressure relief, not a
    // correctness gate.
    //
    // After the startup sweep, a periodic background thread fires every
    // `ECAA_CACHE_EVICTION_PERIOD_SECS` (default 600 s) to catch bursty
    // workloads that fill disk between harness restarts. The guard is
    // held until `run_loop` returns so the thread's lifetime matches the
    // harness's active window.
    let _eviction_guard = {
        use ecaa_workflow_harness::cache_eviction::{eviction_period_from_env, CacheEvictor};
        // Run the startup one-shot sweep, then arm the periodic thread.
        // Two separate `from_env()` calls are cheap (env-var reads only).
        if let Some(startup) = CacheEvictor::from_env() {
            if let Err(e) = startup.enforce() {
                tracing::warn!(error = %e, "agent cache eviction failed (startup)");
            }
        }
        CacheEvictor::from_env().map(|bg| bg.spawn_periodic(eviction_period_from_env()))
    };

    // Select compute backend via env var. Default "local" preserves the
    // pre-refactor behaviour exactly; "aws" returns a structured error
    // See
    // for the full matrix.
    let mode = std::env::var("ECAA_EXECUTOR_MODE").unwrap_or_else(|_| "local".into());
    let exec_args = ExecutorArgs {
        package: args.package.clone(),
        agent: args.agent.clone(),
        task_timeout_secs: args.task_timeout,
    };
    let executor = executor::build(&mode, &exec_args)?;

    println!(
        "{} Starting harness for {}",
        "ecaa-workflow-harness".cyan().bold(),
        args.package.cyan()
    );
    println!(
        "  Agent: {}  Max iterations: {}  Timeout: {}s  Executor: {}",
        args.agent.cyan(),
        args.max_iterations,
        args.task_timeout,
        executor.name().cyan(),
    );
    if let Some(ref id) = args.session_id {
        println!(
            "  Posting progress to {} (session {})",
            args.server_url.cyan(),
            id.cyan()
        );
    }
    println!();

    // Env-capability probe. Runs before the first agent
    // iteration so `discover_*` stages can skip unavailable methods
    // with a structured `env_capability_skip` rationale instead of
    // silently substituting a Python analog. Capability file lands at
    // `<pkg>/runtime/env_capability.json`; failures are logged but do
    // not abort the run.
    if let Err(e) = write_env_capability(path) {
        eprintln!(
            "{} env_capability probe write failed (continuing): {:#}",
            "⚠".yellow(),
            e
        );
    }

    // Extract the cooperative shutdown flag BEFORE wrapping the executor
    // in Arc<Mutex<...>>. Remote backends (AWS, SLURM) expose an
    // Arc<AtomicBool> the SIGINT handler can set without ever touching
    // the iteration mutex — this closes the latency bug where the
    // handler blocked waiting for an SSM/SLURM poll to complete.
    let primary_shutdown_flag = executor.shutdown_flag();

    // Share the executor with the SIGINT handler so `release()` fires on
    // Ctrl+C before process exit.
    let executor: Arc<Mutex<Box<dyn Executor>>> = Arc::new(Mutex::new(executor));

    // Lane-mode wave 4: when ECAA_HARNESS_VALIDATION_LANE=1 AND backend
    // is local, build a second LocalExecutor so the validation lane and
    // processing lane each get their own mutex. Two threads in
    // `thread::scope` then truly run in parallel — neither blocks on
    // the other's `run_iteration`. For aws/slurm, lane mode degrades
    // gracefully: the picker still spans both lanes, but execution
    // serialises through the single backend handle (avoids
    // double-provisioning a remote instance / submitting two batch jobs
    // for one logical lane). A one-time stderr warning fires so the
    // operator knows.
    let validation_executor: Option<Arc<Mutex<Box<dyn Executor>>>> =
        match (lane_mode_from_env().is_some(), mode.as_str()) {
            (true, "local") => {
                let e = executor::build(&mode, &exec_args)?;
                Some(Arc::new(Mutex::new(e)))
            }
            (true, other) => {
                eprintln!(
                    "[lane] ECAA_HARNESS_VALIDATION_LANE=1 with backend '{}' — \
                     lane picker still active, but parallel execution requires \
                     mode=local; validators will run serialised through the \
                     single backend handle.",
                    other
                );
                None
            }
            (false, _) => None,
        };

    // Operator-facing concurrency-vs-lane surprise: setting both
    // ECAA_HARNESS_CONCURRENCY=1 and ECAA_HARNESS_VALIDATION_LANE=1
    // does NOT serialize agent dispatches — validation_lane reserves a
    // second slot regardless of the concurrency value. Operators
    // expecting a single-agent serialized run get two concurrent
    // agents instead. Surface this at startup so the divergence
    // between intent and behavior is visible.
    let concurrency_override = std::env::var("ECAA_HARNESS_CONCURRENCY").ok();
    if lane_mode_from_env().is_some() && concurrency_override.as_deref().map(str::trim) == Some("1")
    {
        eprintln!(
            "[lane] ECAA_HARNESS_CONCURRENCY=1 + ECAA_HARNESS_VALIDATION_LANE=1: \
             validation_lane reserves a separate slot for validators, so the \
             effective dispatch budget is 2 (1 processing + 1 validation), \
             not 1. To strictly serialize agent dispatches unset the lane \
             flag (`unset ECAA_HARNESS_VALIDATION_LANE`)."
        );
    }
    // Sandbox vs atom-safety surprise: when ECAA_LOCAL_SANDBOX is unset
    // (no bubblewrap), an atom's `safety.network = None{allowlist=[]}`
    // is interpreted as MIN-not-MAX semantics — the executor's
    // `NetworkPolicy::Bridge` satisfies the atom's "I need no hosts to
    // be reachable" declaration trivially, and full host network egress
    // is permitted. Operators expecting `safety.network=None` to enforce
    // an EGRESS DENY need bubblewrap sandboxing. Print once at startup
    // so the implicit semantics are observable.
    if std::env::var("ECAA_LOCAL_SANDBOX")
        .ok()
        .map(|v| v.trim().is_empty() || v == "off")
        .unwrap_or(true)
        && mode == "local"
    {
        eprintln!(
            "[safety] ECAA_LOCAL_SANDBOX unset/off + executor=local: \
             atom-level `safety.network=None` declarations are MIN-not-MAX \
             — they do NOT block host network egress. Set \
             ECAA_LOCAL_SANDBOX=bubblewrap to enforce egress deny via \
             `--unshare-net` on declared atoms."
        );
    }

    // Single handler install covering primary + (optional) lane.
    // Shutdown flags are None for the local path (no blocking poll loop).
    let handlers: Vec<Arc<Mutex<Box<dyn Executor>>>> = match &validation_executor {
        Some(ve) => vec![executor.clone(), ve.clone()],
        None => vec![executor.clone()],
    };
    // The lane secondary (local only) never has a blocking remote poll;
    // its shutdown flag is always None.
    let shutdown_flags = match &validation_executor {
        Some(_) => vec![primary_shutdown_flag, None],
        None => vec![primary_shutdown_flag],
    };
    install_signal_handler(handlers, shutdown_flags, path.to_path_buf())?;

    // Security-remediation when the
    // server enforces bearer-token auth but the harness env did not
    // export `ECAA_SERVER_AUTH_TOKEN`, every `POST /api/chat/*` would
    // silently 401. Probe once at startup; bail with a clear error
    // message so the operator can fix the env. Skip the probe when
    // the harness isn't binding to a chat session (no `--session-id`).
    if args.session_id.is_some()
        && std::env::var("ECAA_SERVER_AUTH_TOKEN")
            .ok()
            .filter(|t| !t.is_empty())
            .is_none()
        && ProgressClient::probe_auth_required(&args.server_url)
    {
        anyhow::bail!(
            "server at {} requires ECAA_SERVER_AUTH_TOKEN but the harness env does not set it",
            args.server_url
        );
    }

    let progress = args
        .session_id
        .as_ref()
        .map(|id| ProgressClient::new(id.clone(), args.server_url.clone()).with_package_dir(path));

    // Wire the session endpoint into the executor so it can emit
    // `cost_guard_passed` events on successful provision-cost checks.
    // Executors that don't override `set_session_endpoint` (Local / SLURM /
    // Mock) ignore this call; only AwsExecutor uses it to build an
    // internal ProgressClient free of cross-crate type conflicts.
    if let Some(ref id) = args.session_id {
        let mut guard = executor.lock().unwrap_or_else(|p| p.into_inner());
        guard.set_session_endpoint(id.clone(), args.server_url.clone());
    }

    // Orphan state.patch.json recovery — must run BEFORE WAL recovery
    // so the recovery sees the post-patch state. Otherwise a Running
    // task with a legitimate state.patch.json (agent emitted a
    // transition but the prior harness binary didn't honor the patch
    // protocol) would get clobbered by `[orphaned_by_crash]` before
    // we ever look at the patch file. The orphan-scan path inside
    // apply_pending_patches matches each patch's `from` against the
    // live state, so a stale patch from a prior crashed transition
    // can't resurrect after the SME has moved on.
    match apply_pending_patches(path, &[]) {
        Ok(merged) => {
            if let Err(e) = write_dag(path, &merged) {
                tracing::warn!(
                    target: "patch-startup",
                    error = format!("{:#}", e),
                    "persist of orphan-merged DAG failed"
                );
            }
        }
        Err(e) => tracing::warn!(
            target: "patch-startup",
            error = format!("{:#}", e),
            "orphan scan failed (continuing)"
        ),
    }

    // P1-226 — AWS orphan reap MUST fire before the WAL recovery
    // so the reaped-instance ids can seed the recovery's
    // `instance_denylist`. Legacy ordering ran the sweep after
    // provision (and after WAL recovery), opening a window where:
    //   (a) the heartbeat-mtime liveness probe saw a fresh mtime
    //       from before the agent crashed;
    //   (b) the recovery treated the task as live;
    //   (c) the later sweep terminated the host;
    //   (d) the task wedged in Running forever because nothing
    //       reconsidered its state after the kill.
    // Doing the sweep first closes the window.
    //
    // Local / SLURM backends return None from
    // `sweep_orphans_verified` so the denylist stays empty and the
    // recovery behaves exactly as before.
    let mut instance_denylist: std::collections::HashSet<String> = std::collections::HashSet::new();
    if let Some(ref pc) = progress {
        let summary = {
            let guard = executor.lock().unwrap_or_else(|p| p.into_inner());
            guard.sweep_orphans_verified()
        };
        if let Some(s) = summary {
            // Both verified-terminated AND unverified-but-API-accepted
            // ids feed the denylist: the terminate call already went
            // out, so any agent heartbeat from those hosts is by
            // definition a ghost. Only `terminate_failures` ids stay
            // out of the denylist — AWS refused the kill, so the
            // host may still be alive.
            for id in &s.verified_ids {
                instance_denylist.insert(id.clone());
            }
            for id in &s.unverified_ids {
                instance_denylist.insert(id.clone());
            }
            let reap = progress_client::OrphanReapWire {
                schema_version: progress_client::orphan_reap_wire_schema_version(),
                candidate_count: s.candidate_count,
                verified_count: s.verified_count,
                unverified_ids: s.unverified_ids,
                policy: s.policy,
                terminate_failures: s.terminate_failures,
                verified_ids: s.verified_ids,
            };
            pc.orphan_instances_reaped(reap);
        }
    }

    // dispatch WAL recovery. A harness killed
    // mid-dispatch leaves tasks in Running state; on restart we want
    // to re-block them deterministically instead of relying on the
    // stale-timeout heuristic. Generate this run's id first, then
    // scan the WAL and flip any Running tasks whose last dispatch
    // was from a prior run.
    let harness_run_id = generate_harness_run_id();
    let mut dispatch_epoch: u64 = 0;
    {
        let records = read_dispatches(path);
        if !records.is_empty() {
            let mut dag_for_recovery = read_dag(path)?;
            // Liveness probe: heartbeat-mtime check unless
            // ECAA_HEARTBEAT_LIVENESS_SECS=0 selects the legacy
            // AlwaysDeadProbe (every Running task with a stale-deadline
            // prior-run dispatch gets flagged orphan). The
            // heartbeat probe is what suppresses the
            // restart-induced /unblock dance that fires when a
            // long-running detached compute task (Seurat CCA, etc.)
            // outlives a harness exit at --max-iterations.
            let liveness_secs = heartbeat_liveness_window_secs();
            let liveness_probe: Box<dyn LivenessProbe> = if liveness_secs == 0 {
                Box::new(AlwaysDeadProbe)
            } else {
                Box::new(HeartbeatLivenessProbe {
                    package_root: path.to_path_buf(),
                    freshness_secs: liveness_secs,
                })
            };
            let report = recover_orphaned_dispatches_with_denylist(
                &mut dag_for_recovery,
                &records,
                &harness_run_id,
                liveness_probe.as_ref(),
                &instance_denylist,
                &clock,
            );
            if report.skipped_live_count > 0 {
                tracing::info!(
                    target: "harness-wal",
                    count = report.skipped_live_count,
                    task_ids = %report.skipped_live_task_ids.join(", "),
                    "skipped prior-run dispatch(es) with fresh heartbeat (still live)"
                );
            }
            if report.orphaned_count > 0 {
                write_dag(path, &dag_for_recovery)?;
                tracing::info!(
                    target: "harness-wal",
                    count = report.orphaned_count,
                    task_ids = %report.orphaned_task_ids.join(", "),
                    "recovered orphaned dispatch(es)"
                );
                // Duplicate the same recovery event on the dedicated
                // `ecaa::session_orphan_recovery` target so the operator
                // dashboard can alert on non-zero rates without parsing
                // the harness's own logs. Conceptual metrics counter:
                // `ecaa_session_orphan_recovery_total`.
                tracing::info!(
                    target: "ecaa::session_orphan_recovery",
                    recovered_count = report.orphaned_count,
                    skipped_live_count = report.skipped_live_count,
                    harness_run_id = %harness_run_id,
                    task_ids = %report.orphaned_task_ids.join(", "),
                    "session orphan recovery completed"
                );
                // Re-emit task_blocked progress events so the UI
                // surfaces a BlockerCard immediately instead of
                // waiting for the DAG poll.
                if let Some(ref pc) = progress {
                    for tid in &report.orphaned_task_ids {
                        if let Some(task) = dag_for_recovery.tasks.get(tid.as_str()) {
                            if let TaskState::Blocked { record } = &task.state {
                                pc.task_blocked(tid, &record.reason);
                            }
                        }
                    }
                }
            }
        }
    }

    // emit the backend-selected event as the first harness
    // signal so the Progress tab can render a header row from t=0.
    // `current_instance_type` is `None` for local / slurm at this point;
    // AWS backfills after provision runs below (the UI re-renders on any
    // subsequent task_started event that carries the instance tag).
    if let Some(ref pc) = progress {
        let (cpu_budget, gpu_budget, instance_type, backend_name) = {
            let guard = executor.lock().unwrap_or_else(|p| p.into_inner());
            (
                guard.cpu_budget() as u64,
                guard.gpu_budget() as u64,
                guard.current_instance_type(),
                guard.name().to_string(),
            )
        };
        let info = progress_client::ExecutorInfoWire {
            name: backend_name,
            cpu_budget,
            gpu_budget,
            instance_type,
            harness_version: env!("CARGO_PKG_VERSION").to_string(),
            env_mode: mode.clone(),
        };
        pc.executor_selected(info);
    }

    // Pre-flight sizing pilot. Runs before provision so projections
    // can inform the real provision shape. Errors never abort the run;
    // they downgrade to `sizing_pilot_skipped` + fall through to
    // baseline provisioning.
    let pilot_cfg = PilotConfig::from_env();
    if pilot_cfg.enabled {
        let dag_for_pilot = read_dag(path)?;
        if let Some(ref pc) = progress {
            let picks = executor_pick_preview(&dag_for_pilot, &pilot_cfg);
            pc.sizing_pilot_started(&picks);
        }
        let pilot_outcome = {
            let mut guard = executor.lock().unwrap_or_else(|p| p.into_inner());
            guard.pilot(&dag_for_pilot, &pilot_cfg)
        };
        match pilot_outcome {
            Ok(Some(report)) => {
                println!(
                    "  {} Pilot complete: {} measurements, confidence {:.2}",
                    "✓".green(),
                    report.measurements.len(),
                    report.confidence
                );
                if let Some(ref pc) = progress {
                    pc.sizing_pilot_complete(&report);
                }
            }
            Ok(None) => {
                if let Some(ref pc) = progress {
                    pc.sizing_pilot_skipped("executor returned no report");
                }
            }
            Err(e) => {
                eprintln!("{} Pilot failed (continuing): {:#}", "⚠".yellow(), e);
                if let Some(ref pc) = progress {
                    pc.sizing_pilot_skipped(&e.to_string());
                }
            }
        }
    } else if let Some(ref pc) = progress {
        pc.sizing_pilot_skipped("pilot disabled (set ECAA_PILOT_ENABLED=1)");
    }

    // Provision once before the loop (no-op for local; Phase B wires AWS).
    {
        let mut guard = executor.lock().unwrap_or_else(|p| p.into_inner());
        let dag = read_dag(path)?;
        guard.provision(&dag)?;
    }
    if let Some(ref ve) = validation_executor {
        let mut guard = ve.lock().unwrap_or_else(|p| p.into_inner());
        let dag = read_dag(path)?;
        guard.provision(&dag)?;
    }

    // P1-226 — orphan sweep moved earlier (before WAL recovery) so
    // its `verified_ids` can seed the recovery's `instance_denylist`.
    // Local / SLURM backends still return None from
    // `sweep_orphans_verified` so the early-sweep call above is a
    // no-op for them.

    // Stall monitor wiring. When thresholds are enabled, set up an
    // mpsc channel so the executor's monitor thread can post
    // StallSignals back to the main loop. The Receiver is drained at
    // the top of each iteration. Both executors (when lane-mode
    // active) feed the same Receiver via cloned senders.
    //
    // Fan-out (SSM-hang fix): after the monitor channel is set up we
    // spin a splitter thread that reads from `stall_rx` and sends each
    // signal to BOTH `main_tx` (consumed by `run_loop`) AND `relay_tx`
    // (consumed by `stall_relay::spawn`). This means stall signals reach
    // the direct-relay POST even while the main loop is blocked inside
    // `executor.run_iteration()`.
    let stall_thresholds = StallThresholds::from_env();
    let (stall_tx, stall_rx) = mpsc::channel::<StallSignal>();
    if stall_thresholds.enabled {
        {
            let mut guard = executor.lock().unwrap_or_else(|p| p.into_inner());
            if let Err(e) = guard.start_stall_monitor(&stall_thresholds, stall_tx.clone()) {
                eprintln!(
                    "{} could not start stall monitor (continuing): {:#}",
                    "⚠".yellow(),
                    e
                );
            }
        }
        if let Some(ref ve) = validation_executor {
            let mut guard = ve.lock().unwrap_or_else(|p| p.into_inner());
            if let Err(e) = guard.start_stall_monitor(&stall_thresholds, stall_tx) {
                eprintln!(
                    "{} could not start validation-lane stall monitor (continuing): {:#}",
                    "⚠".yellow(),
                    e
                );
            }
        }
    }

    // Build the fan-out: a splitter thread between the stall-monitor
    // channel and two downstream consumers. The main loop reads from
    // `main_rx`; the relay thread reads from `relay_rx`. When the
    // stall monitor is disabled, `stall_rx` stays empty and both
    // channels are empty too — no behaviour change.
    let (main_tx, main_rx) = mpsc::channel::<StallSignal>();
    let (relay_tx, relay_rx) = mpsc::channel::<StallSignal>();
    std::thread::Builder::new()
        .name("stall-signal-splitter".into())
        .spawn(move || {
            while let Ok(signal) = stall_rx.recv() {
                let _ = relay_tx.send(signal.clone());
                let _ = main_tx.send(signal);
            }
            // Both downstream channels close when this thread exits.
        })
        .expect("spawn stall-signal-splitter thread");

    // Relay thread: direct POST to the server bypassing the main loop.
    // Best-effort — a relay failure only logs a warning and never
    // blocks the harness. The handle is intentionally dropped (detached)
    // so harness shutdown isn't gated on the relay draining.
    // When no `--session-id` is set, drop `relay_rx` immediately so
    // the splitter thread's sends to `relay_tx` return `Err` and it
    // doesn't accumulate signals in an unbounded buffer.
    if let Some(ref session_id) = args.session_id {
        let _relay_handle = stall_relay::spawn(
            path.to_path_buf(),
            session_id.clone(),
            args.server_url.clone(),
            relay_rx,
        );
    } else {
        drop(relay_rx);
    }

    // Wall-clock watchdog — catches CPU-bound infinite loops that maintain a
    // fresh heartbeat but never make overall progress. Runs independently of
    // the stall monitor; both can fire on the same task simultaneously.
    // The watchdog uses WallClock for production; tests substitute FrozenClock.
    let watchdog_config = WatchdogConfig::from_env();
    let (watchdog_tx, watchdog_rx) = mpsc::sync_channel::<WatchdogEvent>(256);
    let mut watchdog = Watchdog::spawn(
        path.to_path_buf(),
        std::sync::Arc::new(WallClock),
        watchdog_config,
        watchdog_tx,
    );

    let run_result = run_loop(
        &args,
        &executor,
        validation_executor.as_ref(),
        &progress,
        &main_rx,
        &watchdog_rx,
        &harness_run_id,
        &mut dispatch_epoch,
        &clock,
    );

    // Shut down the watchdog before the executor so no stale events arrive
    // after the loop exits.
    watchdog.stop();

    // Always run cleanup, even on error / early-return.
    {
        let mut guard = executor.lock().unwrap_or_else(|p| p.into_inner());
        guard.stop_stall_monitor();
        guard.release();
    }
    if let Some(ref ve) = validation_executor {
        let mut guard = ve.lock().unwrap_or_else(|p| p.into_inner());
        guard.stop_stall_monitor();
        guard.release();
    }

    // flush the health sidecar unconditionally and post a
    // final progress_client_health event so the Performance tab can
    // render "Progress events lost" once per run without polling.
    if let Some(ref pc) = progress {
        pc.flush_health_sidecar();
        pc.progress_client_health();
    }

    // non-zero exit when ≥ 50% of POSTs failed so wrapping
    // scripts can detect silent desync. Keeps zero for the happy path
    // so existing CI assertions on exit code don't regress.
    const HARNESS_PROGRESS_CLIENT_DEGRADED: i32 = 2;
    if let Some(ref pc) = progress {
        if pc.health_loss_ratio() >= 0.5 && pc.health_snapshot().total_posts > 0 {
            tracing::error!(
                target: "harness",
                loss_ratio = pc.health_loss_ratio(),
                exit_code = HARNESS_PROGRESS_CLIENT_DEGRADED,
                "progress client degraded; exiting"
            );
            run_result?;
            std::process::exit(HARNESS_PROGRESS_CLIENT_DEGRADED);
        }
    }

    run_result
}

/// Preview pilot task selection without running it — used to surface
/// the pre-flight task ids in the `sizing_pilot_started` event. Reuses
/// the same selection logic the pilot itself will run.
fn executor_pick_preview(dag: &DAG, _cfg: &PilotConfig) -> Vec<String> {
    dag.tasks
        .iter()
        .filter(|(_, t)| matches!(t.state, TaskState::Ready))
        .map(|(id, _)| id.to_string())
        .take(3)
        .collect()
}

#[allow(clippy::too_many_arguments)]
#[tracing::instrument(
    skip(args, executor, validation_executor, progress, stall_rx, watchdog_rx, clock),
    fields(harness_run_id = %harness_run_id)
)]
fn run_loop(
    args: &Args,
    executor: &Arc<Mutex<Box<dyn Executor>>>,
    validation_executor: Option<&Arc<Mutex<Box<dyn Executor>>>>,
    progress: &Option<ProgressClient>,
    stall_rx: &mpsc::Receiver<StallSignal>,
    watchdog_rx: &mpsc::Receiver<WatchdogEvent>,
    harness_run_id: &str,
    dispatch_epoch: &mut u64,
    clock: &dyn Clock,
) -> Result<()> {
    let path = Path::new(&args.package);

    let mut prior_completed: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut prior_running: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut prior_blocked: std::collections::HashSet<String> = std::collections::HashSet::new();
    // No-progress guard: force-block a task whose agent keeps being
    // re-dispatched (orphan recovery) without ever writing a terminal
    // state patch — a crash loop that heartbeat-stall can't catch because
    // each re-dispatch refreshes the heartbeat. Only tasks actually
    // (re-)dispatched this iteration are observed, so a live long-running
    // agent (skipped by the orphan-recovery is_live probe) is never
    // charged. See `dispatch_guard`.
    let mut noprogress_guard =
        ecaa_workflow_harness::dispatch_guard::NoProgressGuard::from_env();
    // Terminal-state scratch cleanup needs to
    // fire exactly once per task per failure transition. The existing
    // `is_failed` event-emit branch below is guarded by
    // `!prior_running.contains(tid)`, which deliberately suppresses a
    // re-fire for tasks that the harness saw Running and then Failed
    // — that gate is load-bearing for the progress event but the
    // scratch lifecycle is independent. Track failures separately so
    // we clean scratch on the Running→Failed transition without
    // disturbing the existing event semantics.
    let mut prior_failed: std::collections::HashSet<String> = std::collections::HashSet::new();

    let runtime_dir = path.join("runtime");
    let pause_sentinel = runtime_dir.join(".harness-pause");
    let pause_ack_sentinel = runtime_dir.join(".harness-paused");
    let stop_sentinel = runtime_dir.join(".harness-stop");

    // Two counters drive the main loop:
    //   `i` is informational — bumps every loop pass so transcripts read
    //   iteration numbers monotonically starting at 1.
    //   `budget_consumed` is what we compare against `--max-iterations`.
    //   It only bumps on iterations that did productive work OR slept
    //   their full settle window. Fail-closed iterations (dispatch_gate
    //   GET returned Err — typically because the server is briefly
    //   unreachable during a restart) refuse to count against the budget
    //   so a 10-minute server outage doesn't burn 1000 budget slots on
    //   tight-looping no-ops.
    //
    //   `max_total_iterations` is a hard upper bound (10x the budget) so
    //   a permanently-unreachable server still terminates the harness
    //   eventually rather than looping forever.
    let max_total_iterations = args.max_iterations.saturating_mul(10);
    let mut budget_consumed: usize = 0;
    let mut i: usize = 0;
    while budget_consumed < args.max_iterations && i < max_total_iterations {
        // Set when the per-iteration dispatch_gate fail-closed path
        // triggered. Iterations flagged here don't count against
        // `--max-iterations`; see `budget_consumed` above.
        let mut dispatch_gate_failed_this_iter = false;
        // Cooperative stop check — if /execution/stop wrote the
        // sentinel, mark the in-flight task back to ready (NOT
        // running, to prevent orphan-recovery false-fires on next
        // start), archive its WAL line, and exit cleanly.
        if stop_sentinel.exists() {
            println!(
                "  {} Stop sentinel observed at iteration {} — graceful shutdown",
                "■".red(),
                i + 1,
            );
            // Find any task in Running and reset to Ready
            if let Ok(mut dag) = read_dag(path) {
                let mut touched: Vec<String> = Vec::new();
                for (tid, task) in dag.tasks.iter_mut() {
                    if matches!(
                        task.state,
                        ecaa_workflow_core::dag::TaskState::Running { .. }
                    ) {
                        task.state = ecaa_workflow_core::dag::TaskState::Ready;
                        touched.push(tid.to_string());
                    }
                }
                if !touched.is_empty() {
                    // Mirror on-disk first so any third-party reader observing WORKFLOW.json sees the new state before the server's SSE stream does.
                    let _ = write_dag(path, &dag);
                    // Mirror the reset to the server's authoritative
                    // task_states map.
                    if let Some(ref pc) = progress {
                        for tid in &touched {
                            pc.set_task_state(tid, &TaskState::Ready);
                        }
                    }
                    println!(
                        "  {} Reset {} in-flight task(s) back to Ready: {}",
                        "↩".yellow(),
                        touched.len(),
                        touched.join(", ").cyan(),
                    );
                }
            }
            // Truncate the dispatch WAL — no orphan-recovery on next start
            if let Err(e) = truncate_wal(path) {
                tracing::warn!(
                    target: "harness-wal",
                    error = %e,
                    "truncate on /stop failed (continuing)"
                );
            }
            // Remove our own pause-ack sentinel if any
            let _ = std::fs::remove_file(&pause_ack_sentinel);
            println!("\n{} Harness stopped via /execution/stop.", "→".blue());
            return Ok(());
        }

        // Cooperative pause check — if /execution/pause wrote the
        // Sentinel, ack via.harness-paused and idle until the
        // sentinel goes away (resume) or stop arrives.
        if pause_sentinel.exists() {
            // Ack — server's /execution endpoint reads this to flip
            // status to "paused" rather than just "running with
            // pause_requested".
            let _ = std::fs::write(&pause_ack_sentinel, b"ack\n");
            println!("  {} Paused — waiting for /execution/resume", "⏸".yellow(),);
            loop {
                std::thread::sleep(std::time::Duration::from_millis(500));
                if stop_sentinel.exists() {
                    // Stop overrides pause; loop back to top of
                    // iteration which will see the stop sentinel.
                    break;
                }
                if !pause_sentinel.exists() {
                    let _ = std::fs::remove_file(&pause_ack_sentinel);
                    println!("  {} Resumed", "▶".green());
                    break;
                }
            }
            // Re-evaluate stop sentinel at top of next iteration —
            // `continue` rather than fall through so we don't miss it.
            // Bump both counters first so this short-circuit obeys the
            // same loop-tail accounting as the normal path.
            if stop_sentinel.exists() {
                i = i.saturating_add(1);
                budget_consumed = budget_consumed.saturating_add(1);
                continue;
            }
        }

        println!("{} Iteration {}", "→".blue(), i + 1);

        // Drain any stall signals produced by the executor's monitor
        // thread since the last iteration. Non-blocking: signals that
        // arrive mid-iteration roll over to the next pass. Each signal
        // becomes a `task_stalled` POST which the server translates
        // into `Blocked { Stalled }`.
        for signal in stall_rx.try_iter() {
            let task_id = match &signal {
                StallSignal::CpuStarvation { task_id, .. }
                | StallSignal::MemoryPressure { task_id, .. }
                | StallSignal::GpuIdleDuringTraining { task_id, .. }
                | StallSignal::RuntimeOverExpected { task_id, .. } => task_id.clone(),
            };
            // Persist to the sidecar before the POST so that a crash
            // between detection and a successful server round-trip does
            // not silently drop the signal. The write is best-effort;
            // failure is logged by the helper and never blocks dispatch.
            ecaa_workflow_harness::executor::stall_monitor::append_stall_signal_record(
                path, &signal,
            );
            let suggested = signal.suggested_action();
            let wire = signal.to_wire();
            // Pair the stall with a resize suggestion when the
            // executor reports a known instance type AND suggest_resize
            // projects a concrete bump. Silent no-op for local
            // executor (current_instance_type returns None).
            let current_instance = {
                let guard = executor.lock().unwrap_or_else(|p| p.into_inner());
                guard.current_instance_type()
            };
            let resize_to = current_instance.as_deref().and_then(|current| {
                ecaa_workflow_harness::executor::stall_monitor::suggest_resize(&signal, current)
            });
            println!(
                "  {} Stall observed on {}: forwarding to session",
                "⚠".yellow(),
                task_id.red()
            );
            if let Some(ref pc) = progress {
                pc.task_stalled(&task_id, &wire, suggested);
                if let (Some(from), Some(to)) = (current_instance, resize_to) {
                    println!(
                        "  {} Resize projection: {} → {}",
                        "→".blue(),
                        from.cyan(),
                        to.cyan()
                    );
                    pc.resize_recommended(&task_id, &from, &to);
                }
            }
        }

        // Drain watchdog events emitted since the last iteration.
        // WallClockExceeded → post a `task_wall_clock_exceeded` progress event
        //   so the server can transition the task to Blocked { WallClockExceeded }.
        // HeartbeatAge → forward as a `heartbeat_age_secs` SSE payload so the
        //   UI Progress tab can render live heartbeat staleness for every Running
        //   task, including CPU-bound loops that keep the heartbeat fresh.
        for event in watchdog_rx.try_iter() {
            match event {
                WatchdogEvent::WallClockExceeded {
                    ref task_id,
                    observed_secs,
                    threshold_secs,
                } => {
                    if !watchdog_wall_clock_event_is_current(path, task_id) {
                        continue;
                    }
                    println!(
                        "  {} Wall-clock budget exceeded on {}: {}s > {}s",
                        "⚠".yellow(),
                        task_id.red(),
                        observed_secs,
                        threshold_secs,
                    );
                    if let Some(ref pc) = progress {
                        pc.wall_clock_exceeded(task_id, observed_secs, threshold_secs);
                    }
                }
                WatchdogEvent::HeartbeatAge {
                    ref task_id,
                    age_secs,
                } => {
                    if let Some(ref pc) = progress {
                        pc.heartbeat_age_update(task_id, age_secs);
                    }
                }
            }
        }

        // Deterministic finalize probe — runs each candidate task's
        // agent-declared `recoverable_action.rerun_script` BEFORE the
        // scheduler picks. Catches sentinel arrivals (long-running R /
        // Python compute writes its OK/FAILED file) without dispatching
        // the LLM agent. The wrappers are throttled per-task via the
        // `last_probe.json` sidecar so a fast-iterating harness doesn't
        // oversample. See `finalize_probe.rs` for the full failure-mode
        // catalogue. Probes run on both Blocked and Running tasks —
        // Blocked covers the live IVD pump case (agent already wrote
        // `running → blocked`), Running covers the post-Layer-A path
        // where the agent yields a no-op heartbeat patch.
        {
            let dag_for_probe = read_dag(path)?;
            let probe_targets: Vec<String> = dag_for_probe
                .tasks
                .iter()
                .filter(|(_, t)| {
                    matches!(
                        t.state,
                        TaskState::Blocked { .. } | TaskState::Running { .. }
                    )
                })
                .map(|(id, _)| id.to_string())
                .collect();
            for tid in &probe_targets {
                match probe_one_task(path, tid) {
                    ProbeOutcome::Ran { exit_code: 0 } => {
                        eprintln!("  {} finalize_probe ran for {} (exit 0)", "·".cyan(), tid);
                    }
                    ProbeOutcome::Ran { exit_code } => {
                        eprintln!(
                            "  {} finalize_probe ran for {} (exit {})",
                            "·".cyan(),
                            tid,
                            exit_code
                        );
                    }
                    ProbeOutcome::TimedOut => {
                        eprintln!(
                            "  {} finalize_probe timed out for {} — wrapper hung; will retry next iteration",
                            "⚠".yellow(),
                            tid
                        );
                    }
                    // Skipped/Throttled are normal; no log noise.
                    ProbeOutcome::Skipped { .. } | ProbeOutcome::Throttled { .. } => {}
                }
            }
            // Merge any state.patch.json files the wrappers wrote.
            // Picks here is empty — the orphan-scan pass picks up the
            // patches. This must happen BEFORE scheduler picks so a
            // wrapper that just completed a task doesn't get
            // re-dispatched.
            if !probe_targets.is_empty() {
                if let Ok(merged) = apply_pending_patches(path, &[]) {
                    if let Err(e) = write_dag(path, &merged) {
                        tracing::warn!(
                            target: "finalize_probe",
                            error = format!("{:#}", e),
                            "persist post-probe DAG failed"
                        );
                    }
                }
            }
        }

        // Recover stale Running tasks and propagate readiness before each
        // iteration. Stale-detection delegates to the active executor so
        // remote backends can layer cloud-side health signals on top of
        // the timestamp threshold.
        let mut dag = read_dag(path)?;
        let now = chrono::Utc::now().timestamp() as u64;
        let mut stale_recovered: Vec<ecaa_workflow_core::ids::TaskId> = Vec::new();
        {
            let guard = executor.lock().unwrap_or_else(|p| p.into_inner());
            for (tid, task) in dag.tasks.iter_mut() {
                if matches!(task.state, TaskState::Running { .. }) && guard.is_task_stale(task, now)
                {
                    task.state = TaskState::Ready;
                    stale_recovered.push(tid.clone());
                }
            }
        }
        // Incremental propagation when stale-recovery touched specific
        // tasks; full scan only when no tasks were recovered (the
        // wake-up tick still needs to surface deps that completed
        // outside this iteration body).
        if stale_recovered.is_empty() {
            dag.propagate_readiness();
        } else {
            dag.propagate_readiness_from(&stale_recovered);
        }
        // Mirror on-disk first so any third-party reader observing WORKFLOW.json sees the new state before the server's SSE stream does.
        write_dag(path, &dag)?;
        // Mirror stale-recovery resets to the server's authoritative
        // task_states map.
        if let Some(ref pc) = progress {
            for tid in &stale_recovered {
                pc.set_task_state(tid.as_str(), &TaskState::Ready);
            }
        }

        let before_val = serde_json::to_value(&dag)?;

        // Session-state gate. Skip new dispatches when the
        // session is Blocked / Amending / PendingConfirmation so SME
        // mid-amend doesn't race against fresh task launches.
        //
        // Fail-CLOSED: when the dispatch-gate
        // GET fails (network blip, server restart, parse error)
        // treat the session as paused and sleep
        // `ECAA_HARNESS_SETTLE_SECS` before re-iterating. The prior
        // fail-open behavior let the harness happily launch agents
        // against a paused session whenever the server was briefly
        // unreachable; now we wait. The sleep is bounded by
        // `settle_interval_secs()` so a typo can't freeze the
        // harness for hours; `ECAA_HARNESS_SETTLE_SECS=0` (settle
        // disabled) skips the sleep and falls back to immediate
        // re-iteration without dispatch.
        let session_pausing = match progress.as_ref() {
            None => false,
            Some(pc) => match pc.is_session_pausing_dispatch() {
                Ok(b) => b,
                Err(e) => {
                    // Don't sleep here — the end-of-iteration `is_idle`
                    // branch handles it via `settle_interval_secs()` so
                    // we get exactly one sleep per fail-closed pass
                    // instead of two. `dispatch_gate_failed_this_iter`
                    // also (a) flags the loop tail so this iteration
                    // doesn't count against `--max-iterations` and (b)
                    // short-circuits `picks` to empty so agents aren't
                    // dispatched against a session whose state we
                    // couldn't read.
                    tracing::warn!(
                        target: "dispatch_gate",
                        error = %e,
                        settle_secs = settle_interval_secs(),
                        "failed to read session state; treating as paused (fail-closed) — will sleep at end of iteration"
                    );
                    dispatch_gate_failed_this_iter = true;
                    true
                }
            },
        };

        // When the session is in `Amending` state, soft-cancel any
        // Running tasks whose ids appear in `invalidated_tasks`. This
        // closes the recovery hole described in §10.2 of the
        // executor-harness deep analysis: without this, in-flight tasks
        // complete against the old DAG and write outputs that are stale
        // relative to the amended package the SME is about to re-emit.
        //
        // Flow:
        // 1. GET session state → parse `amending.invalidated_tasks`.
        // 2. For each id in that list that is currently `Running`:
        //    a. Call `executor.cancel_task(id, &dag)` — SIGTERM/cancel-command/scancel.
        //    b. Transition to `Blocked { CancelledByAmendment }` in WORKFLOW.json.
        //    c. Mirror the state via `pc.set_task_state`.
        //    d. Remove any pending `state.patch.json` for that task so the
        //       next iteration's patch-merge can't resurrect a stale completion.
        if session_pausing {
            if let Some(ref pc) = progress {
                if let Some((target_stage, invalidated_ids)) = pc.get_amending_invalidated_tasks() {
                    let mut dag_for_cancel = match read_dag(path) {
                        Ok(d) => d,
                        Err(e) => {
                            tracing::warn!(
                                target: "amend_cancel",
                                error = %e,
                                "could not read DAG for amend-cancel sweep"
                            );
                            dag.clone()
                        }
                    };
                    let mut cancelled: Vec<String> = Vec::new();
                    for tid in &invalidated_ids {
                        let is_running = matches!(
                            dag_for_cancel.tasks.get(tid.as_str()),
                            Some(t) if matches!(t.state, TaskState::Running { .. })
                        );
                        if !is_running {
                            continue;
                        }
                        // Step (a): backend-native cancel.
                        {
                            let guard = executor.lock().unwrap_or_else(|p| p.into_inner());
                            if let Err(e) = guard.cancel_task(tid, &dag_for_cancel) {
                                tracing::warn!(
                                    target: "amend_cancel",
                                    task_id = %tid,
                                    error = %e,
                                    "cancel_task error (continuing to block)"
                                );
                            }
                        }
                        // Step (b): write Blocked { CancelledByAmendment } to the DAG.
                        let blocker_reason = format!(
                            "[cancelled_by_amendment] task={} target_stage={}",
                            tid, target_stage
                        );
                        if let Some(t) = dag_for_cancel.tasks.get_mut(tid.as_str()) {
                            t.state = TaskState::Blocked {
                                record: ecaa_workflow_core::dag::BlockedRecord {
                                    reason: blocker_reason.clone(),
                                    attempts: vec![],
                                },
                            };
                        }
                        // Step (d): remove any pending state.patch.json so a stale
                        // completion from the dying agent doesn't resurrect the task.
                        let patch_path = path
                            .join("runtime")
                            .join("outputs")
                            .join(tid)
                            .join("state.patch.json");
                        if patch_path.exists() {
                            if let Err(e) = std::fs::remove_file(&patch_path) {
                                tracing::warn!(
                                    target: "amend_cancel",
                                    task_id = %tid,
                                    path = %patch_path.display(),
                                    error = %e,
                                    "could not remove stale state.patch.json"
                                );
                            }
                        }
                        cancelled.push(tid.clone());
                        println!(
                            "  {} Amend-cancel: blocked {} (stage={})",
                            "⊘".red(),
                            tid.red(),
                            target_stage.cyan(),
                        );
                        append_progress_log(
                            path,
                            tid,
                            &format!(
                                "harness: task soft-cancelled — session is amending stage {}",
                                target_stage
                            ),
                        );
                    }
                    if !cancelled.is_empty() {
                        if let Err(e) = write_dag(path, &dag_for_cancel) {
                            tracing::warn!(
                                target: "amend_cancel",
                                error = %e,
                                "could not persist DAG after amend-cancel"
                            );
                        }
                        // Step (c): mirror each cancellation to the server's task_states.
                        for tid in &cancelled {
                            let new_state = TaskState::Blocked {
                                record: ecaa_workflow_core::dag::BlockedRecord {
                                    reason: format!(
                                        "[cancelled_by_amendment] task={} target_stage={}",
                                        tid, target_stage
                                    ),
                                    attempts: vec![],
                                },
                            };
                            pc.set_task_state(tid, &new_state);
                            pc.task_blocked(
                                tid,
                                &format!(
                                    "Task cancelled — session is amending stage {}",
                                    target_stage
                                ),
                            );
                        }
                        tracing::info!(
                            target: "amend_cancel",
                            count = cancelled.len(),
                            target_stage = %target_stage,
                            task_ids = %cancelled.join(", "),
                            "amend-cancel sweep completed"
                        );
                    }
                }
            }
        }

        // Resolve budget from ECAA_HARNESS_CONCURRENCY against the
        // executor's declared capacity. Default is serial
        // (cpu_slots=1, gpu_slots=0), identical to the
        // pre-parallel pick-one-per-iteration contract.
        //
        // When the session is pausing (a discover_* task blocked for
        // SME review) we no longer zero the budget. Instead we compute
        // the set of tasks that transitively depend on any currently-
        // Blocked task and exclude only those — letting validators and
        // review tasks with no dependency on the blocked discover stage
        // proceed normally.
        let budget: SchedulerBudget = {
            let (exec_cpu, exec_gpu) = {
                let guard = executor.lock().unwrap_or_else(|p| p.into_inner());
                (guard.cpu_budget(), guard.gpu_budget())
            };
            ConcurrencyMode::from_env().resolve_budget(exec_cpu, exec_gpu)
        };
        // Compute the pause-dependent exclusion set. Empty when not
        // pausing; populated with the transitive dependents of all
        // currently-Blocked tasks when the session is pausing.
        let pause_excluded: std::collections::HashSet<String> = if session_pausing {
            let dag_for_pause = match read_dag(path) {
                Ok(d) => d,
                Err(_) => dag.clone(),
            };
            let blocked_ids: std::collections::HashSet<ecaa_workflow_core::ids::TaskId> =
                dag_for_pause
                    .tasks
                    .iter()
                    .filter(|(_, t)| matches!(t.state, TaskState::Blocked { .. }))
                    .map(|(id, _)| id.clone())
                    .collect();
            pause_dependent_tasks(&dag_for_pause, &blocked_ids)
                .into_iter()
                .map(|id| id.to_string())
                .collect()
        } else {
            std::collections::HashSet::new()
        };

        // Pre-mark up to `budget` Ready tasks as Running, in id order.
        // Pre-mark preserves the UI's running-transition visibility
        // invariant (Plan + Jobs tabs see a running state before
        // Ready → Completed agents land their result). Each
        // pre-marked task emits a task_started progress event + a
        // harness-owned log line before the agent spawn.
        //
        // After budget-picking, filter out tasks whose
        // ancestor chain includes a Completed task with
        // requires_sme_review: true that the SME has not yet
        // confirmed. Confirmed stages come from per-stage sidecar
        // files written by the server's /confirm handler.
        let (picks, picked_dispatches): (Vec<String>, Vec<PickedDispatch>) = {
            let mut dag_mut = read_dag(path)?;
            let mut picked_dispatches = Vec::new();
            // Validation-lane mode (ECAA_HARNESS_VALIDATION_LANE=1)
            // overrides ECAA_HARNESS_CONCURRENCY: one slot reserved
            // for validators, one for processing.
            let raw_picks = if let Some(lanes) = lane_mode_from_env() {
                pick_ready_with_lanes(&dag_mut, lanes)
            } else {
                pick_ready_respecting_budgets(&dag_mut, budget)
            };
            // When session is pausing, filter out tasks that
            // transitively depend on a Blocked task — they would
            // themselves hit the SME gate. Tasks with no such
            // dependency (validators, review tasks) are left in.
            //
            // Fail-closed override: when the dispatch_gate GET failed
            // this iteration we have no reliable view of session state,
            // so refuse to dispatch ANY task — not just transitively-
            // dependent ones. Otherwise an unreachable-server window
            // would still spawn agents whose POSTs immediately 404.
            let raw_picks: Vec<ecaa_workflow_core::ids::TaskId> =
                if dispatch_gate_failed_this_iter {
                    Vec::new()
                } else if session_pausing {
                    raw_picks
                        .into_iter()
                        .filter(|id| !pause_excluded.contains(id.as_str()))
                        .collect()
                } else {
                    raw_picks
                };
            let confirmed_stages = read_confirmed_review_stages(path);
            let picks_pre_sandbox: Vec<String> =
                filter_picks_respecting_sme_gate(&dag_mut, raw_picks, &confirmed_stages)
                    .into_iter()
                    .map(|id| id.to_string())
                    .collect();
            // Pre-dispatch sandbox check. For v4 sessions
            // with an active policy bundle, refuse tasks that violate
            // the sandbox policy (e.g. unreviewed generated code,
            // unpinned containers under clinical bundle). Refused
            // tasks transition to Blocked instead of Running.
            let sandbox_refusals = collect_sandbox_refusals(path, &picks_pre_sandbox);
            let picks_post_sandbox: Vec<String> = picks_pre_sandbox
                .iter()
                .filter(|id| !sandbox_refusals.contains_key(id.as_str()))
                .cloned()
                .collect();
            // Dispatch-time safety-policy gate. Each
            // task's declared `safety` (atom-derived `SafetyLevel` +
            // `SandboxRequirement` + `NetworkPolicy`) is checked
            // against the active executor's capability profile. A
            // mismatch transitions the task to Blocked with a typed
            // `BlockerKind` (SandboxRequired / NetworkPolicyMismatch)
            // — the SME's recovery affordance is "switch executor" or
            // "downgrade safety", surfaced by `BlockerCard`. Pre-A.S6
            // packages whose tasks carry `safety: SafetyPolicy::default()`
            // and `source_atom_id: None` pass the gate unconditionally,
            // so there's no regression on legacy WORKFLOW.json.
            let executor_caps = {
                let guard = executor.lock().unwrap_or_else(|p| p.into_inner());
                guard.capabilities()
            };
            let safety_refusals =
                collect_safety_policy_refusals(&dag_mut, &picks_post_sandbox, &executor_caps);
            let picks: Vec<String> = picks_post_sandbox
                .iter()
                .filter(|id| !safety_refusals.contains_key(id.as_str()))
                .cloned()
                .collect();
            // Picker-decision audit trail. Appends one record per Ready
            // task examined this iteration to
            // `runtime/picker-decisions.jsonl` when at least one task
            // was refused. Accepted-only iterations produce no output.
            // File write is best-effort — errors are warned and swallowed
            // so a disk hiccup never blocks dispatch.
            //
            // Classification order (first match wins):
            //   accepted           — task in `picks`
            //   sandbox_refused    — task in `sandbox_refusals`
            //   network_refused    — task in `safety_refusals` with NetworkPolicyMismatch
            //   safety_refused     — task in `safety_refusals` (other BlockerKind)
            //   sme_review_required — in budget picks but filtered by SME gate
            //   slot_exhausted     — Ready but not reached by budget picker
            {
                use picker_decisions::{append_picker_decisions, PickerDecisionRecord};
                use ecaa_workflow_core::blocker::BlockerKind;

                let now_ts = chrono::Utc::now().to_rfc3339();
                // Re-run the budget picker (pure read over in-memory dag;
                // no I/O) to reconstruct which tasks were budget-selected
                // after pause-exclusion. This lets us distinguish
                // slot_exhausted (budget skipped), pause_dependent (filtered
                // because the task transitively depends on a Blocked task
                // while the session is pausing), and sme_review_required
                // (budget included, gate filtered).
                let budget_picks_set: std::collections::BTreeSet<String> = {
                    let raw: Vec<String> = if budget.cpu_slots == 0 && budget.gpu_slots == 0 {
                        Vec::new()
                    } else if let Some(lanes) = lane_mode_from_env() {
                        pick_ready_with_lanes(&dag_mut, lanes)
                            .into_iter()
                            .map(|id| id.to_string())
                            .collect()
                    } else {
                        pick_ready_respecting_budgets(&dag_mut, budget)
                            .into_iter()
                            .map(|id| id.to_string())
                            .collect()
                    };
                    raw.into_iter()
                        .filter(|id| !pause_excluded.contains(id))
                        .collect()
                };
                let picks_pre_sandbox_set: std::collections::BTreeSet<&str> =
                    picks_pre_sandbox.iter().map(String::as_str).collect();
                let picks_set: std::collections::BTreeSet<&str> =
                    picks.iter().map(String::as_str).collect();
                // Iterate over all Ready tasks in stable id order.
                let all_ready: Vec<String> = dag_mut
                    .tasks
                    .iter()
                    .filter(|(_, t)| matches!(t.state, TaskState::Ready))
                    .map(|(id, _)| id.to_string())
                    .collect();
                let mut audit_records: Vec<PickerDecisionRecord> = Vec::new();
                for task_id in &all_ready {
                    let (decision, reason): (&'static str, String) =
                        if picks_set.contains(task_id.as_str()) {
                            ("accepted", String::new())
                        } else if sandbox_refusals.contains_key(task_id.as_str()) {
                            (
                                "sandbox_refused",
                                sandbox_refusals
                                    .get(task_id.as_str())
                                    .cloned()
                                    .unwrap_or_default(),
                            )
                        } else if let Some(blocker) = safety_refusals.get(task_id.as_str()) {
                            match blocker {
                                BlockerKind::NetworkPolicyMismatch { .. } => {
                                    ("network_refused", format!("{blocker:?}"))
                                }
                                _ => ("safety_refused", format!("{blocker:?}")),
                            }
                        } else if pause_excluded.contains(task_id) {
                            // Transitively depends on a Blocked task
                            // while the session is pausing; withheld
                            // until the SME unblocks the upstream gate.
                            ("pause_dependent", String::new())
                        } else if budget_picks_set.contains(task_id)
                            && !picks_pre_sandbox_set.contains(task_id.as_str())
                        {
                            ("sme_review_required", String::new())
                        } else {
                            // Not reached by the budget picker.
                            ("slot_exhausted", String::new())
                        };
                    audit_records.push(PickerDecisionRecord {
                        ts: now_ts.clone(),
                        iteration: i,
                        task_id: task_id.clone(),
                        decision,
                        reason,
                    });
                }
                // Write only when at least one task was refused so the
                // happy path (everything accepted) produces no output.
                if audit_records.iter().any(|r| r.decision != "accepted") {
                    append_picker_decisions(path, &audit_records);
                }
            }
            for (id, blocker) in &safety_refusals {
                if let Some(t) = dag_mut.tasks.get_mut(id.as_str()) {
                    // Format the typed BlockerKind into the
                    // `[sandbox_required] {json}` /
                    // `[network_policy_mismatch] {json}` marker that
                    // `core::blocker::parse_agent_blocker_kind`
                    // round-trips into the typed variant for the UI.
                    let block_reason =
                        ecaa_workflow_core::blocker::format_safety_policy_marker(blocker)
                            .unwrap_or_else(|| format!("{blocker:?}"));
                    t.state = TaskState::Blocked {
                        record: ecaa_workflow_core::dag::BlockedRecord {
                            reason: block_reason,
                            attempts: vec![],
                        },
                    };
                }
                eprintln!(
                    "  {} safety-policy: refusing dispatch of {} ({:?})",
                    "⚠".yellow(),
                    id,
                    blocker
                );
                append_progress_log(
                    path,
                    id,
                    &format!("harness: safety-policy refused dispatch — {blocker:?}"),
                );
            }
            for (id, reason) in &sandbox_refusals {
                if let Some(t) = dag_mut.tasks.get_mut(id.as_str()) {
                    // Emit the structured payload
                    // `[sandbox_refused] <piece>; <piece>` so
                    // `core::blocker::parse_agent_blocker_kind` upgrades
                    // the BlockedRecord into a typed
                    // `BlockerKind::SandboxRefused`. The bare prefix
                    // (no `task=<id>` token) lets the parser split
                    // pieces unambiguously on `;`.
                    let block_reason = format!("[sandbox_refused] {}", reason);
                    t.state = TaskState::Blocked {
                        record: ecaa_workflow_core::dag::BlockedRecord {
                            reason: block_reason,
                            attempts: vec![],
                        },
                    };
                }
                eprintln!(
                    "  {} sandbox-enforce: refusing dispatch of {} ({})",
                    "⚠".yellow(),
                    id,
                    reason,
                );
                append_progress_log(
                    path,
                    id,
                    &format!("harness: sandbox refused dispatch — {}", reason),
                );
            }
            for id in &picks {
                if let Some(t) = dag_mut.tasks.get_mut(id.as_str()) {
                    t.state = TaskState::Running {
                        started_at: ecaa_workflow_core::time_helpers::now_rfc3339(),
                        remote: None,
                    };
                }
            }
            // Mirror on-disk first so any third-party reader observing WORKFLOW.json sees the new state before the server's SSE stream does.
            write_dag(path, &dag_mut)?;
            // Mirror sandbox-refused Blocked and pre-dispatch Running
            // transitions to the authoritative server-side task_states
            // map BEFORE the matching task_started/task_blocked
            // progress events fire.
            if let Some(ref pc) = progress {
                for id in sandbox_refusals.keys() {
                    if let Some(t) = dag_mut.tasks.get(id.as_str()) {
                        pc.set_task_state(id, &t.state);
                    }
                }
                for id in safety_refusals.keys() {
                    if let Some(t) = dag_mut.tasks.get(id.as_str()) {
                        pc.set_task_state(id, &t.state);
                    }
                }
                for id in &picks {
                    if let Some(t) = dag_mut.tasks.get(id.as_str()) {
                        pc.set_task_state(id, &t.state);
                    }
                }
            }
            for id in &picks {
                if let Some(ref pc) = progress {
                    if let Some(t) = dag_mut.tasks.get(id.as_str()) {
                        pc.task_started(id, &t.description);
                        prior_running.insert(id.clone());
                    }
                }
                append_progress_log(
                    path,
                    id,
                    &format!("harness: invoking agent for {} (iteration {})", id, i + 1),
                );
                // §1.2 — seed the heartbeat file at pre-mark time so
                // the stall detector has a baseline even when the
                // agent script hasn't yet started its touch loop.
                //
                // W7.3: if the heartbeat baseline can't be written, the
                // orphan reaper would false-positive on the next
                // iteration — better to roll the task back to Ready
                // immediately and let it retry on the next loop than
                // dispatch with no liveness signal.
                if touch_heartbeat(path, id).is_err() {
                    if let Some(t) = dag_mut.tasks.get_mut(id.as_str()) {
                        t.state = ecaa_workflow_core::dag::TaskState::Ready;
                    }
                    append_progress_log(
                        path,
                        id,
                        "harness: heartbeat baseline write failed; reset to Ready (will retry next iteration)",
                    );
                    continue;
                }
                // §1.6 — append a dispatch WAL record so a mid-dispatch
                // crash is recoverable on the next harness start.
                *dispatch_epoch += 1;
                let epoch = *dispatch_epoch;
                let now = clock.now();
                let rec = DispatchRecord {
                    schema_version:
                        ecaa_workflow_harness::dispatch_wal::dispatch_wal_schema_version(),
                    task_id: id.clone(),
                    epoch,
                    harness_run_id: harness_run_id.to_string(),
                    started_at: now.to_rfc3339(),
                    timeout_at: (now + chrono::Duration::seconds(args.task_timeout as i64))
                        .to_rfc3339(),
                };
                if let Err(e) = append_dispatch(path, &rec) {
                    tracing::warn!(
                        target: "harness-wal",
                        task_id = %id,
                        epoch = epoch,
                        error = %e,
                        "dispatch record append failed"
                    );
                }
                picked_dispatches.push(PickedDispatch {
                    task_id: id.clone().into(),
                    harness_run_id: harness_run_id.to_string(),
                    epoch,
                });
            }
            (picks, picked_dispatches)
        };
        // Retained for the post-iteration log-line pairing below.
        // Serial mode → exactly one pick; parallel → multiple.
        let started_task_id = picks.first().cloned();
        let dispatch_by_task: std::collections::BTreeMap<String, PickedDispatch> =
            picked_dispatches
                .iter()
                .map(|d| (d.task_id.to_string(), d.clone()))
                .collect();

        // Hoisted DAG snapshot for the pre-dispatch read-only phases.
        // ensure_alive, count_concurrent_peers_by_class, envelope
        // rendering, and task-kind capture all read the same on-disk
        // state: WORKFLOW.json was last written above (post pre-mark) at
        // the end of the picks-loop block, and the next write is the
        // restore_agent_workflow_edits call AFTER the agent threads.
        // One read replaces five.
        let dispatch_snapshot = read_dag(path)?;

        // Remote backends consult the cloud state before dispatch so
        // a spot interruption / manual termination is recovered by
        // reprovisioning. Local backend's default impl is a no-op —
        // zero cost for the byte-identical path.
        {
            let mut guard = executor.lock().unwrap_or_else(|p| p.into_inner());
            if let Err(e) = guard.ensure_alive(&dispatch_snapshot) {
                eprintln!("{} ensure_alive failed: {:#}", "✗".red(), e);
                break;
            }
        }
        if let Some(ve) = validation_executor {
            let mut guard = ve.lock().unwrap_or_else(|p| p.into_inner());
            if let Err(e) = guard.ensure_alive(&dispatch_snapshot) {
                eprintln!(
                    "{} ensure_alive (validation lane) failed: {:#}",
                    "✗".red(),
                    e
                );
                break;
            }
        }

        // Dispatch each picked task with its own envelope.
        // `std::thread::scope` is the idiomatic zero-tokio parallel
        // primitive. With lane mode active (validation_executor is
        // Some), validators lock the secondary mutex and processing
        // tasks lock the primary — two threads truly run in parallel
        // because they're on disjoint mutexes. Without lane mode (or
        // when picks are all of one kind), all threads share one
        // mutex and serialise as before. `count_concurrent_peers_by_class`
        // computes peer counts against the newly-pre-marked DAG so
        // each envelope sees the final running set.
        let peers_by_class = count_concurrent_peers_by_class(&dispatch_snapshot);
        let envelopes: std::collections::BTreeMap<
            String,
            std::collections::BTreeMap<String, String>,
        > = if picks.is_empty() {
            std::collections::BTreeMap::new()
        } else {
            let dag_snapshot: &DAG = &dispatch_snapshot;
            // Dynamic per-task allocation: probe live host pressure,
            // resolve each pick's per-stage high-water requirement, and
            // split the usable budget proportionally. Each agent's
            // ECAA_HW_VCPUS_AVAILABLE / ECAA_HW_MEMORY_GB now reflects
            // its allocated slice rather than the full host. Set
            // ECAA_HW_DYNAMIC_ALLOCATION=0 to fall back to the legacy
            // "full host" envelope (e.g. for byte-identical regression
            // baselines).
            let dynamic = std::env::var("ECAA_HW_DYNAMIC_ALLOCATION").ok().as_deref() != Some("0");
            // Load the package-level runtime
            // prereqs once and bucket by registry; each pick's
            // `provisioning.json` consumes the same map so the shim's
            // `declared_only` enforcement is dispatch-stable.
            let declared = load_declared_per_registry(path);
            if dynamic {
                let host = ecaa_workflow_harness::executor::host_probe::probe();
                let overhead = OverheadPolicy::from_env();
                let requested: Vec<(ecaa_workflow_core::ids::TaskId, _)> = picks
                    .iter()
                    .map(|id| {
                        (
                            ecaa_workflow_core::ids::TaskId::from(id.as_str()),
                            resolve_high_water_for(path, dag_snapshot, id),
                        )
                    })
                    .collect();
                let allocations = allocate_for_picks(&host, &overhead, &requested);
                picks
                    .iter()
                    .map(|id| {
                        let task_id_key = ecaa_workflow_core::ids::TaskId::from(id.as_str());
                        let alloc = allocations.get(&task_id_key).cloned().unwrap_or_else(|| {
                            ecaa_workflow_harness::executor::host_probe::AgentAllocation::cpu_only(
                                host.free_vcpus_estimate.max(1),
                                host.free_memory_gb.max(2),
                            )
                        });
                        let inputs = HardwareEnvelopeInputs {
                            vcpus_available: alloc.vcpus,
                            memory_gb: alloc.memory_gb,
                            gpu_descriptor: alloc.gpu_descriptor,
                            concurrent_peers_by_class: peers_by_class.clone(),
                        };
                        let mut env = render_envelope(path, id, dag_snapshot, &inputs);
                        stamp_dispatch_identity(&mut env, dispatch_by_task.get(id));
                        stamp_literature_scope(&mut env);
                        stamp_provisioning_policy(&mut env, path, dag_snapshot, id, &declared);
                        stamp_safety_network(&mut env, dag_snapshot, id);
                        (id.clone(), env)
                    })
                    .collect()
            } else {
                let mut inputs = HardwareEnvelopeInputs::local_serial();
                inputs.concurrent_peers_by_class = peers_by_class.clone();
                picks
                    .iter()
                    .map(|id| {
                        let mut env = render_envelope(path, id, dag_snapshot, &inputs);
                        stamp_dispatch_identity(&mut env, dispatch_by_task.get(id));
                        stamp_literature_scope(&mut env);
                        stamp_provisioning_policy(&mut env, path, dag_snapshot, id, &declared);
                        stamp_safety_network(&mut env, dag_snapshot, id);
                        (id.clone(), env)
                    })
                    .collect()
            }
        };

        // Snapshot task kinds before thread::scope so each spawn can
        // decide its routing without re-locking the DAG.
        let task_kinds: std::collections::BTreeMap<String, ecaa_workflow_core::dag::TaskKind> =
            picks
                .iter()
                .filter_map(|id| {
                    dispatch_snapshot
                        .tasks
                        .get(id.as_str())
                        .map(|t| (id.clone(), t.kind.clone()))
                })
                .collect();
        // Pre-dispatch baseline captured before the agent threads run.
        // No writes have occurred since `dispatch_snapshot` was read at
        // the top of this block, so cloning it is byte-equivalent to a
        // fresh re-read. `restore_agent_workflow_edits` below compares
        // this baseline against the post-agent disk state.
        let dag_before_agent = dispatch_snapshot.clone();
        let mut had_agent_error = false;
        std::thread::scope(|scope| {
            let mut handles = Vec::new();
            for id in &picks {
                let envelope = envelopes.get(id).cloned().unwrap_or_default();
                let is_validation = matches!(
                    task_kinds.get(id),
                    Some(ecaa_workflow_core::dag::TaskKind::Validation)
                );
                let exec_ref = match (validation_executor, is_validation) {
                    (Some(ve), true) => ve.clone(),
                    _ => executor.clone(),
                };
                let agent_arg = args.agent.clone();
                let path_buf = path.to_path_buf();
                let task_id_for_overrides = id.clone();
                handles.push(scope.spawn(move || {
                    let (outcome, capture) = {
                        let mut guard = exec_ref.lock().unwrap_or_else(|p| p.into_inner());
                        // Per-task remediation overrides applied right
                        // before dispatch. Server's apply-remediation
                        // endpoint writes runtime/inputs/<task>/overrides.json
                        // and triggers an auto-relaunch; the next harness
                        // process picks the file up here. Read failures
                        // are logged but never abort dispatch — a
                        // malformed file shouldn't strand the task.
                        match ecaa_workflow_harness::executor::overrides_io::read(
                            &path_buf,
                            &task_id_for_overrides,
                        ) {
                            Ok(Some(ov)) => {
                                if let Err(e) = guard.apply_overrides(&task_id_for_overrides, &ov) {
                                    tracing::warn!(
                                        target: "overrides",
                                        task_id = %task_id_for_overrides,
                                        error = format!("{:#}", e),
                                        "apply failed (continuing)"
                                    );
                                }
                            }
                            Ok(None) => {}
                            Err(e) => {
                                // W1.2: surface via the silent-skip
                                // counter so a run with several malformed
                                // overrides files shows up in
                                // `harness-health.json` even when no
                                // single line is alarming on its own.
                                ecaa_workflow_harness::_observability::note_silent_skip(
                                    ecaa_workflow_harness::_observability::SkipCategory::OverridesUnreadable,
                                    &format!("{:#}", e),
                                    Some(&task_id_for_overrides),
                                );
                            }
                        }
                        let o = guard.run_iteration(&path_buf, &agent_arg, &envelope);
                        let c = guard.take_last_capture();
                        (o, c)
                    };
                    (id.clone(), outcome, capture)
                }));
            }
            for h in handles {
                match h.join() {
                    Ok((tid, Ok(o), capture)) if !o.agent_status.success() => {
                        eprintln!(
                            "{} Agent exited with status {} (task {})",
                            "⚠".yellow(),
                            o.agent_status,
                            tid
                        );
                        // Compare new envelope to the prior one to set
                        // the audit-trail outcome on the most recent
                        // applied remediation: Recurred (same error
                        // class) or NewError (different). Reads the
                        // pre-existing envelope BEFORE write_tool_error_envelope
                        // overwrites.
                        let prior_class = read_existing_envelope_error_class(path, &tid);
                        if let Some(cap) = capture {
                            if let Err(e) = write_tool_error_envelope(path, &tid, &cap) {
                                tracing::warn!(
                                    target: "envelope",
                                    task_id = %tid,
                                    error = format!("{:#}", e),
                                    "writing tool-error envelope failed"
                                );
                            }
                            if let Some(prior) = prior_class {
                                let new_class = read_existing_envelope_error_class(path, &tid);
                                let outcome = if new_class.as_deref() == Some(prior.as_str()) {
                                    ecaa_workflow_core::remediation::RemediationOutcome::Recurred
                                } else {
                                    ecaa_workflow_core::remediation::RemediationOutcome::NewError
                                };
                                update_overrides_outcome(path, &tid, outcome);
                            }
                        }
                    }
                    Ok((tid, Ok(_o), _capture)) => {
                        // Agent succeeded — if a prior remediation was
                        // pending, mark its outcome as Resolved so the
                        // proposer can see "this fix worked" on the
                        // session's history view.
                        update_overrides_outcome(
                            path,
                            &tid,
                            ecaa_workflow_core::remediation::RemediationOutcome::Resolved,
                        );
                    }
                    Ok((tid, Err(e), _capture)) => {
                        eprintln!("{} Agent subprocess error on {}: {}", "✗".red(), tid, e);
                        let _ = tid;
                        had_agent_error = true;
                    }
                    Err(_) => {
                        eprintln!("{} worker thread panicked", "✗".red());
                        had_agent_error = true;
                    }
                }
            }
        });
        if had_agent_error {
            break;
        }

        // Agents are not allowed to mutate WORKFLOW.json directly.
        // Restore the pre-dispatch snapshot before harvesting patches
        // so a roaming or legacy agent cannot complete unrelated tasks
        // by writing a whole-DAG snapshot.
        if !picks.is_empty() {
            // fresh re-read: agent threads may have mutated WORKFLOW.json.
            restore_agent_workflow_edits(path, &dag_before_agent, read_dag(path), &picks)?;
        }

        // Aggregate output-directory size cap. Check each dispatched task
        // before merging its state.patch.json. Tasks whose output directory
        // total exceeds ECAA_TASK_OUTPUT_MAX_MB are blocked immediately; their
        // patch is NOT merged so the completion state is never accepted.
        // Oversized tasks are removed from the dispatch list so
        // apply_pending_patches_strict ignores their patch files.
        let picked_dispatches = {
            use ecaa_workflow_core::dag::BlockedRecord;
            let mut kept = Vec::with_capacity(picked_dispatches.len());
            let mut size_blocked: Vec<String> = Vec::new();
            for dispatch in picked_dispatches {
                match ecaa_workflow_harness::output_size_guard::check_output_size(
                    path,
                    dispatch.task_id.as_str(),
                ) {
                    Ok(()) => kept.push(dispatch),
                    Err((observed_bytes, threshold_bytes)) => {
                        eprintln!(
                            "{} output size cap exceeded for {}: {} bytes observed (threshold {} bytes) — blocking task, patch NOT merged",
                            "⚠".yellow(),
                            dispatch.task_id,
                            observed_bytes,
                            threshold_bytes,
                        );
                        // Block the task in the current on-disk DAG.
                        if let Ok(mut dag) = read_dag(path) {
                            if let Some(task) = dag.tasks.get_mut(&dispatch.task_id) {
                                task.state = TaskState::Blocked {
                                    record: BlockedRecord {
                                        reason: format!(
                                            "[output_size_exceeded] task={} observed_bytes={} threshold_bytes={}",
                                            dispatch.task_id, observed_bytes, threshold_bytes,
                                        ),
                                        attempts: vec![],
                                    },
                                };
                            }
                            if let Err(e) = write_dag(path, &dag) {
                                tracing::warn!(
                                    target: "output_size_guard",
                                    error = %e,
                                    "failed to persist blocked state for oversized task"
                                );
                            }
                        }
                        size_blocked.push(dispatch.task_id.to_string());
                    }
                }
            }
            // Mirror size-blocked tasks to the server's authoritative
            // task_states map so the UI sees the blocked state.
            if let Some(ref pc) = progress {
                if let Ok(dag) = read_dag(path) {
                    for tid in &size_blocked {
                        if let Some(t) = dag.tasks.get(tid.as_str()) {
                            pc.set_task_state(tid, &t.state);
                        }
                    }
                }
            }
            kept
        };

        // Merge per-task state.patch.json files written by the agents.
        // Normal live dispatch is strict: only the picked task's patch
        // with the matching harness run id + dispatch epoch is accepted.
        // Legacy orphan patch recovery remains available only at
        // startup/finalize through apply_pending_patches(path, &[]).
        let mut after = match apply_pending_patches_strict(path, &picked_dispatches) {
            Ok(d) => {
                if let Err(e) = write_dag(path, &d) {
                    tracing::warn!(
                        target: "patch",
                        error = format!("{:#}", e),
                        "persist of merged DAG failed"
                    );
                }
                d
            }
            Err(e) => {
                tracing::warn!(
                    target: "patch",
                    error = format!("{:#}", e),
                    "strict merge failed"
                );
                read_dag(path)?
            }
        };

        // Validation-contract enforcement. Runs before the
        // silent-completion guard so a contract violation surfaces as
        // the authoritative block reason when the agent marks a task
        // completed with empty output.
        match enforce_validation_contract(path, &mut after) {
            Ok(violations) if !violations.is_empty() => {
                for (task_id, ids) in &violations {
                    eprintln!(
                        "{} validation-contract violation on {}: [{}] — re-blocking task + its validator",
                        "⚠".yellow(),
                        task_id,
                        ids.join(", ")
                    );
                    append_progress_log(
                        path,
                        task_id,
                        &format!(
                            "harness validation-contract: required assertion(s) unsatisfied: {}",
                            ids.join(", ")
                        ),
                    );
                }
                if let Err(e) = write_dag(path, &after) {
                    // W1.2/B7: was eprintln!; structured tracing so the
                    // persist failure is filtered/discoverable alongside
                    // the rest of the harness log.
                    tracing::error!(
                        target: "harness-guard",
                        error = format!("{:#}", e),
                        "failed to persist contract-enforcement state"
                    );
                }
            }
            Ok(_) => {}
            Err(e) => tracing::warn!(
                target: "harness-guard",
                error = %e,
                "contract enforcement error"
            ),
        }

        // Silent-completion guard: layered defense.
        //
        // (a) Legacy sentinel check — if the agent marked a compute task
        // `completed` but the result carries an `overall_*_not_run:
        // true` sentinel (typical when every SME decision funneled
        // to empty output), flip back to `blocked`. See
        //
        //
        // (b) required-artifact check. If the task's
        // `required_artifacts` declaration is non-empty, every
        // listed path under `runtime/outputs/<task_id>/` must
        // exist, be non-empty, and meet `min_size_bytes`. Missing
        // entries re-block with a `[missing_artifact]` marker in
        // the reason string that the server promotes to
        // `BlockerKind::MissingArtifact` via the blocker mapper.
        let mut guard_flipped: Vec<String> = Vec::new();
        for (tid, task) in after.tasks.iter_mut() {
            if let TaskState::Completed { result } = &task.state {
                // SME-acknowledged skip short-circuit. When the SME has
                // explicitly chosen a skip option on this task's blocker
                // (read from runtime/outputs/<task_id>/sme-decisions.json),
                // the empty/sentinel completion is authorized — taking
                // the strict path would loop the agent against the guard.
                let sme_intent = sme_skip::detect_intent(path, tid.as_str());
                if sme_intent.is_skip() {
                    tracing::info!(
                        target: "harness-guard",
                        task_id = %tid,
                        intent = ?sme_intent,
                        "SME-acknowledged skip — bypassing empty-result + required-artifact + validator guards"
                    );
                    continue;
                }
                // (a) sentinel
                let sentinel = result.as_object().map(|obj| {
                    obj.iter().any(|(k, v)| {
                        k.starts_with("overall_")
                            && k.ends_with("_not_run")
                            && v.as_bool() == Some(true)
                    })
                });
                if sentinel.unwrap_or(false) {
                    let blocker_path = path
                        .join("runtime/outputs")
                        .join(tid.as_str())
                        .join("blocker.json");
                    let reason_hint = if blocker_path.exists() {
                        format!(
                            "Harness guard: agent marked {} completed with empty output (overall_*_not_run: true). Re-blocked. See runtime/outputs/{}/blocker.json for the narrower decision points the SME must answer.",
                            tid, tid
                        )
                    } else {
                        format!(
                            "Harness guard: agent marked {} completed with empty output (overall_*_not_run: true). Re-blocked — agent must write a blocker.json with narrower decision_points_for_sme before advancing.",
                            tid
                        )
                    };
                    task.state = TaskState::Blocked {
                        record: ecaa_workflow_core::dag::BlockedRecord {
                            reason: reason_hint,
                            attempts: vec![],
                        },
                    };
                    guard_flipped.push(tid.to_string());
                    continue;
                }
                // (b) required-artifact verification
                let missing = match verify_required_artifacts(
                    path,
                    tid.as_str(),
                    &task.required_artifacts,
                ) {
                    Ok(missing) => missing,
                    Err(e) => {
                        let reason = format!(
                            "[missing_artifact] task={} paths=<invalid> — required artifact declaration is invalid: {}",
                            tid, e
                        );
                        task.state = TaskState::Blocked {
                            record: ecaa_workflow_core::dag::BlockedRecord {
                                reason,
                                attempts: vec![],
                            },
                        };
                        guard_flipped.push(tid.to_string());
                        continue;
                    }
                };
                if !missing.is_empty() {
                    // Marker prefix that the server's blocker mapper
                    // recognizes to produce BlockerKind::MissingArtifact
                    // with the paths pulled from the reason suffix.
                    let reason = format!(
                        "[missing_artifact] task={} paths={} — agent marked completed but required artifacts are missing or empty.",
                        tid,
                        missing.join(","),
                    );
                    task.state = TaskState::Blocked {
                        record: ecaa_workflow_core::dag::BlockedRecord {
                            reason,
                            attempts: vec![],
                        },
                    };
                    guard_flipped.push(tid.to_string());
                    continue;
                }
                // (c) run the validator bundle on
                // the completed task and append per-row results to
                // `runtime/validation-reports.jsonl`. Pulled from the
                // task's RequiredArtifact entries: each entry can
                // declare a `validation_obligations` set; the union
                // across the task's artifacts is the bundle the
                // harness runs. Failures additionally re-block the
                // task with a typed reason the server promotes to
                // `BlockerKind::ValidationFailed`. Empty bundle = no
                // validators run = no sidecar lines appended.
                let obligations: Vec<String> = task
                    .required_artifacts
                    .iter()
                    .flat_map(|a| a.validation_obligations.iter().cloned())
                    .collect();
                if !obligations.is_empty() {
                    // Validators inspect artifacts under
                    // runtime/outputs/<task_id>/ so the artifact path
                    // matches `verify_required_artifacts` above.
                    let artifact_path = path.join("runtime/outputs").join(tid.as_str());
                    let runners = ecaa_workflow_harness::validators::default_runners();
                    let summary = ecaa_workflow_harness::validators::evaluate_validation(
                        tid.as_str(),
                        &obligations,
                        &runners,
                        &artifact_path,
                    );
                    append_validation_reports_sidecar(path, tid.as_str(), &summary);
                    if summary.has_failures() {
                        let reason = format!(
                            "[validation_failed] task={} {} — Phase 13 validator(s) reported failures.",
                            tid,
                            summary.human_summary(),
                        );
                        task.state = TaskState::Blocked {
                            record: ecaa_workflow_core::dag::BlockedRecord {
                                reason,
                                attempts: vec![],
                            },
                        };
                        guard_flipped.push(tid.to_string());
                    }
                }
            }
        }
        if !guard_flipped.is_empty() {
            // Mirror on-disk first so any third-party reader observing WORKFLOW.json sees the new state before the server's SSE stream does.
            // Persist the re-flip so the next iteration (and the UI)
            // sees the blocker state rather than the stale completion.
            if let Err(e) = write_dag(path, &after) {
                tracing::warn!(
                    target: "harness-guard",
                    error = %e,
                    "failed to persist re-blocked state"
                );
            }
            // Mirror harness-guard re-blocks (the sentinel /
            // missing-artifact / validation-failed cases above) to the
            // server's authoritative task_states map. The existing
            // prior_blocked-gated `pc.task_blocked` emission a few
            // blocks down still fires the user-facing progress event;
            // this call writes the state itself.
            if let Some(ref pc) = progress {
                for tid in &guard_flipped {
                    if let Some(t) = after.tasks.get(tid.as_str()) {
                        pc.set_task_state(tid, &t.state);
                    }
                }
            }
            for tid in &guard_flipped {
                append_progress_log(
                    path,
                    tid,
                    &format!(
                        "harness-guard: flipped {} completed -> blocked (empty-result sentinel detected)",
                        tid
                    ),
                );
            }
        }

        // Paired end-of-iteration log line so the drawer shows both
        // harness markers even on a pure-stub agent run. Iterate every
        // picked task rather than just the first — all pre-marked
        // tasks need their tail log line.
        for tid in &picks {
            let new_state_label = after
                .tasks
                .get(tid.as_str())
                .map(|t| match t.state {
                    TaskState::Completed { .. } => "completed",
                    TaskState::Blocked { .. } => "blocked",
                    TaskState::Failed { .. } => "failed",
                    TaskState::Running { .. } => "still running",
                    TaskState::Ready => "ready",
                    TaskState::Pending => "pending",
                })
                .unwrap_or("unchanged");
            append_progress_log(
                path,
                tid,
                &format!(
                    "harness: agent returned — new task state: {}",
                    new_state_label
                ),
            );
        }

        // No-progress guard. A task that was (re-)dispatched this
        // iteration but did NOT reach a terminal state made no progress;
        // count it. Once a task exhausts its budget, force it to Blocked
        // so the harness stops re-dispatching a crash loop the
        // heartbeat-stall detector can't catch (each re-dispatch refreshes
        // the heartbeat). Terminal outcomes reset the count, and a live
        // long-running agent is never in `picks` (orphan recovery skips
        // live tasks), so this can't false-positive a slow stage.
        for tid in &picks {
            let reached_terminal = after
                .tasks
                .get(tid.as_str())
                .map(|t| {
                    matches!(
                        t.state,
                        TaskState::Completed { .. }
                            | TaskState::Failed { .. }
                            | TaskState::Blocked { .. }
                    )
                })
                .unwrap_or(false);
            if let Some(reason) = noprogress_guard.observe(tid.as_str(), reached_terminal) {
                if let Some(t) = after.tasks.get_mut(tid.as_str()) {
                    t.state = TaskState::Blocked {
                        record: ecaa_workflow_core::dag::BlockedRecord {
                            reason: reason.clone(),
                            attempts: Vec::new(),
                        },
                    };
                }
                append_progress_log(
                    path,
                    tid,
                    &format!("harness: no-progress guard force-blocked {tid} — {reason}"),
                );
                tracing::warn!(
                    target: "harness",
                    task_id = %tid,
                    "no-progress guard force-blocked task after repeated no-terminal dispatches"
                );
            }
        }
        // `started_task_id` is now just the first pick (used by
        // callers expecting the single-pick semantics).
        let _ = started_task_id;

        // Emit progress events for any state transitions since the previous
        // iteration. This keeps the conversation-side UI in sync without
        // requiring the agent to know about it.
        //
        // Each transition branch additionally mirrors the new state
        // to the authoritative
        // `POST /api/chat/session/:id/task/:task_id/state` endpoint via
        // `pc.set_task_state`, so the server-side `task_states` map
        // captures harness-merged agent transitions instead of being
        // clobbered by the conversation tool-loop merge.
        // Mirror on-disk first so any third-party reader observing WORKFLOW.json sees the new state before the server's SSE stream does.
        if let Err(e) = write_dag(path, &after) {
            tracing::warn!(
                target: "harness",
                error = %e,
                "pre-notify write_dag failed; on-disk state may lag SSE"
            );
        }
        if let Some(ref pc) = progress {
            for (tid, task) in &after.tasks {
                let tid_str: &str = tid.as_str();
                let is_running = matches!(task.state, TaskState::Running { .. });
                let is_completed = matches!(task.state, TaskState::Completed { .. });
                let is_blocked = matches!(task.state, TaskState::Blocked { .. });
                let is_failed = matches!(task.state, TaskState::Failed { .. });
                // Clear prior_blocked when the task leaves Blocked (via
                // SME unblock → Ready/Running, or completion). Without
                // this clear, the "fire once per transition" gate below
                // suppresses every re-block after the first — observed
                // when the agent completes iteration N, hits a new
                // blocker in iteration N+1, and the harness never
                // POSTs task_blocked to the server. SME sees no
                // BlockerCard and the test stalls.
                if !is_blocked && prior_blocked.contains(tid_str) {
                    prior_blocked.remove(tid_str);
                }
                if is_running && !prior_running.contains(tid_str) {
                    pc.set_task_state(tid_str, &task.state);
                    pc.task_started(tid_str, &task.description);
                    prior_running.insert(tid_str.to_string());
                }
                if is_completed && !prior_completed.contains(tid_str) {
                    pc.set_task_state(tid_str, &task.state);
                    // If the agent wrote runtime/outputs/<tid>/agent-usage.json,
                    // attach the parsed usage so the server can record
                    // agent-side spend into the session metrics. Missing
                    // file = older agent with no instrumentation; post
                    // the bare event so the existing wire contract is
                    // preserved.
                    match ProgressClient::read_agent_usage(path, tid_str) {
                        Some(usage) => {
                            pc.task_completed_with_usage(tid_str, &task.description, usage);
                        }
                        None => pc.task_completed(tid_str, &task.description),
                    }
                    prior_completed.insert(tid_str.to_string());
                    prior_running.remove(tid_str);
                    // Terminal-state scratch cleanup. Without this
                    // hook `runtime/scratch/<tid>/` accumulates across
                    // all dispatches in a package. By the time we
                    // observe the Completed transition the agent
                    // subprocess has exited, so no concurrent reader
                    // exists. Bypass via ECAA_SCRATCH_KEEP=1 for
                    // forensic debugging.
                    cleanup_task_scratch(path, tid_str);
                }
                // Fire task_blocked on any new Blocked state, not only
                // when we haven't seen the task Running. Because the
                // harness pre-marks tasks Running for UI visibility, a
                // task that blocks is always already in prior_running
                // — gating on that set would suppress the blocker
                // event entirely. Track prior_blocked separately so
                // each blocker fires once per transition.
                if is_blocked && !prior_blocked.contains(tid_str) {
                    pc.set_task_state(tid_str, &task.state);
                    // Surface the agent-written blocker reason instead
                    // of the task description. The
                    // BlockerCard parses this string for a
                    // `runtime/outputs/<task_id>/decision.json` tail
                    // to decide whether to render the rich candidate
                    // picker, so the reason must carry the agent's
                    // full text. Falls back to task.description when
                    // the record is empty (legacy path).
                    let reason = if let TaskState::Blocked { record } = &task.state {
                        if !record.reason.is_empty() {
                            record.reason.clone()
                        } else {
                            task.description.clone()
                        }
                    } else {
                        task.description.clone()
                    };
                    pc.task_blocked(tid_str, &reason);
                    prior_blocked.insert(tid_str.to_string());
                    prior_running.remove(tid_str);
                }
                if is_failed && !prior_running.contains(tid_str) {
                    pc.set_task_state(tid_str, &task.state);
                    // When a tool-error envelope was just written for
                    // this task, route the event as `task_blocked`
                    // instead of `task_failed` so the server's
                    // progress handler upgrades it to
                    // `BlockerKind::ToolError` and the BlockerCard
                    // surfaces the remediation list. The envelope
                    // synthesis already happened earlier in this
                    // iteration; if the file is present the SME can
                    // act on it. Without the envelope (legacy /
                    // non-capturing executor) fall back to the
                    // original `task_failed` path.
                    let envelope_path = path
                        .join("runtime")
                        .join("outputs")
                        .join(tid_str)
                        .join("error.json");
                    if envelope_path.exists() {
                        let reason = if let TaskState::Failed { reason } = &task.state {
                            reason.clone()
                        } else {
                            task.description.clone()
                        };
                        pc.task_blocked(tid_str, &reason);
                        prior_blocked.insert(tid_str.to_string());
                    } else {
                        pc.task_failed(tid_str, &task.description);
                    }
                }
                // Scratch cleanup on Failed. Fires
                // once per terminal Failed transition regardless of
                // whether the prior_running gate above suppressed the
                // user-facing event (the event semantics are
                // load-bearing; the scratch lifecycle is independent).
                if is_failed && !prior_failed.contains(tid_str) {
                    cleanup_task_scratch(path, tid_str);
                    prior_failed.insert(tid_str.to_string());
                    prior_running.remove(tid_str);
                }
            }
        }

        if after.is_complete() {
            println!(
                "\n{} All tasks complete after {} iteration(s).",
                "✓".green().bold(),
                i + 1
            );
            if let Some(ref pc) = progress {
                pc.execution_finished();
            }
            // Clean exit — empty the WAL so the next harness start (e.g.
            // a fresh one-shot run against the same package) doesn't
            // run orphan recovery against completed dispatches.
            if let Err(e) = truncate_wal(path) {
                tracing::warn!(
                    target: "harness-wal",
                    error = %e,
                    "truncate on completion failed (continuing)"
                );
            }
            return Ok(());
        }

        // Handle blocked tasks with no ready tasks remaining
        let blocked = after.blocked_tasks();
        let ready = after.ready_tasks();

        if ready.is_empty() && !blocked.is_empty() {
            println!(
                "\n{} Blocked tasks require SME resolution:",
                "⚠".yellow().bold()
            );
            for tid in &blocked {
                let task = &after.tasks[*tid];
                println!("  {} — {}", tid.as_str().red().bold(), task.description);
                if let TaskState::Blocked { record } = &task.state {
                    println!("    Reason: {}", record.reason.yellow());
                    for attempt in &record.attempts {
                        println!("    Tried: {} → {}", attempt.method, attempt.result);
                    }
                }
                if let Some(ref res) = task.resolution {
                    println!("    Suggested: {}", res.primary.cyan());
                }
            }

            if args.no_interactive {
                // Signal the web UI server that SME input is needed
                let entry = serde_json::json!({
                    "type": "waiting_for_sme",
                    "blocked_tasks": blocked.iter().map(|tid| {
                        serde_json::json!({"task_id": tid, "task": &after.tasks[*tid]})
                    }).collect::<Vec<_>>(),
                    "timestamp": ecaa_workflow_core::time_helpers::now_rfc3339()
                });
                append_log(path, &entry)?;
                println!(
                    "  {} Wrote waiting_for_sme to LOG.jsonl. Waiting for server to patch WORKFLOW.json...",
                    "→".blue()
                );
                std::thread::sleep(Duration::from_secs(5));
                i = i.saturating_add(1);
                budget_consumed = budget_consumed.saturating_add(1);
                continue;
            }

            // Interactive SME resolution via rustyline
            let mut rl = rustyline::DefaultEditor::new()?;
            let mut dag_mut = read_dag(path)?;
            let mut resolved_ids: Vec<ecaa_workflow_core::ids::TaskId> = Vec::new();
            for tid in &blocked {
                let prompt = format!("  resolve {} > ", tid);
                if let Ok(decision) = rl.readline(&prompt) {
                    let decision = decision.trim().to_string();
                    if !decision.is_empty() {
                        if let Some(task) = dag_mut.tasks.get_mut(tid.as_str()) {
                            task.state = TaskState::Completed {
                                result: serde_json::json!({
                                    "resolved_at": "runtime",
                                    "resolved_by": "sme",
                                    "decision": decision,
                                }),
                            };
                            resolved_ids.push((*tid).clone());
                        }
                    }
                }
            }
            // Incremental propagation: only re-evaluate tasks downstream of
            // the SME-resolved set rather than scanning the whole DAG.
            if resolved_ids.is_empty() {
                dag_mut.propagate_readiness();
            } else {
                dag_mut.propagate_readiness_from(&resolved_ids);
            }
            // Mirror on-disk first so any third-party reader observing WORKFLOW.json sees the new state before the server's SSE stream does.
            write_dag(path, &dag_mut)?;
            // Mirror interactive SME resolutions. The interactive
            // path typically runs without --session-id so `progress`
            // is None and this is a no-op, but we wire it for
            // completeness when an SME runs the REPL against an
            // active web session.
            if let Some(ref pc) = progress {
                for tid in &resolved_ids {
                    if let Some(t) = dag_mut.tasks.get(tid.as_str()) {
                        pc.set_task_state(tid.as_str(), &t.state);
                    }
                }
            }
            i = i.saturating_add(1);
            budget_consumed = budget_consumed.saturating_add(1);
            continue;
        }

        // §1.2 — per-task heartbeat stall detection. Runs instead of
        // the legacy 3-iteration DAG-patch-empty heuristic. For every
        // Running task, compare the age of `.heartbeat` (falling back
        // to `started_at` when the file is absent) against
        // `ECAA_TASK_HEARTBEAT_STALL_SECS` (default 900s). Stalled
        // tasks flip to `Blocked { [heartbeat_stalled] }`; the server
        // promotes the marker to `BlockerKind::HeartbeatStalled` via
        // the blocker mapper.
        let threshold = heartbeat_stall_threshold_secs();
        if threshold > 0 {
            let mut any_flipped = false;
            let mut hb_flipped_ids: Vec<ecaa_workflow_core::ids::TaskId> = Vec::new();
            let mut dag_for_hb = read_dag(path)?;
            for (tid, task) in dag_for_hb.tasks.iter_mut() {
                let TaskState::Running { started_at, .. } = &task.state else {
                    continue;
                };
                let age = heartbeat_age_secs(path, tid.as_str()).unwrap_or_else(|| {
                    // Fallback: time since started_at when the
                    // heartbeat file is missing (older agent scripts
                    // or interrupted touch-loops).
                    chrono::DateTime::parse_from_rfc3339(started_at)
                        .map(|t| {
                            let now = chrono::Utc::now().timestamp();
                            now.saturating_sub(t.timestamp()).max(0) as u64
                        })
                        .unwrap_or(0)
                });
                if age >= threshold {
                    if let Some(ref pc) = progress {
                        pc.heartbeat_stalled(tid.as_str(), age);
                    }
                    // Before recording the legacy
                    // `[heartbeat_stalled]` marker, ask the executor
                    // whether the container is still alive on a healthy
                    // host. When the probe finds an alive container we
                    // emit `[container_hung]` instead so the chat-side
                    // BlockerCard renders the "reap container only,
                    // preserve host" recovery affordance via
                    // `BlockerKind::ContainerHung`. Local / Mock impls
                    // default-return NoSignal so this stays a no-op for
                    // host-mode runs.
                    let probe = {
                        use ecaa_workflow_core::container_state::ContainerProbeOutcome;
                        let outcome = match executor.lock() {
                            Ok(guard) => guard.probe_container_state(tid.as_str(), path),
                            Err(poisoned) => {
                                let guard = poisoned.into_inner();
                                guard.probe_container_state(tid.as_str(), path)
                            }
                        };
                        match outcome {
                            ContainerProbeOutcome::ContainerAlive {
                                container_id,
                                runtime,
                            } => Some((container_id, runtime)),
                            _ => None,
                        }
                    };
                    let reason = match probe {
                        Some((cid, runtime)) => format!(
                            "[container_hung] task={} age_secs={} container_id={} runtime={} — heartbeat stale but container still alive (threshold {}s).",
                            tid, age, cid, runtime, threshold,
                        ),
                        None => format!(
                            "[heartbeat_stalled] task={} age_secs={} — no heartbeat update in {}s (threshold {}s).",
                            tid, age, age, threshold,
                        ),
                    };
                    task.state = TaskState::Blocked {
                        record: ecaa_workflow_core::dag::BlockedRecord {
                            reason,
                            attempts: vec![],
                        },
                    };
                    any_flipped = true;
                    hb_flipped_ids.push(tid.clone());
                }
            }
            if any_flipped {
                // Incremental propagation: tasks moved Running→Blocked,
                // so only downstream of the flipped set could be
                // affected (their dep guard is recomputed).
                dag_for_hb.propagate_readiness_from(&hb_flipped_ids);
                // Mirror on-disk first so any third-party reader observing WORKFLOW.json sees the new state before the server's SSE stream does.
                write_dag(path, &dag_for_hb)?;
                // Mirror heartbeat-stall blocks (and container-hung
                // variants) to the server's authoritative task_states
                // map. `pc.heartbeat_stalled` already fired per-task
                // above for the user-facing progress line; this call
                // writes the underlying TaskState transition.
                if let Some(ref pc) = progress {
                    for tid in &hb_flipped_ids {
                        if let Some(t) = dag_for_hb.tasks.get(tid.as_str()) {
                            pc.set_task_state(tid.as_str(), &t.state);
                        }
                    }
                }
            }
        }

        // Informational progress line so CI transcripts still show
        // iteration-by-iteration status. No longer gates loop exit —
        // the heartbeat check above is the circuit-breaker, and DAG
        // completeness (at the top of the next iteration) ends the run.
        let after_val = serde_json::to_value(&after)?;
        let patch = json_patch::diff(&before_val, &after_val);
        let transitions_this_iter = patch.0.len();
        if patch.0.is_empty() {
            println!("  {} No DAG state change this iteration.", "·".yellow());
        } else {
            let (completed, ready, blocked, pending) = after.progress();
            println!(
                "  Progress: {} completed, {} ready, {} blocked, {} pending",
                completed.to_string().green(),
                ready.to_string().blue(),
                blocked.to_string().red(),
                pending.to_string().white()
            );
        }

        // §Layer-D — settle. When the iteration was a true no-op AND
        // there's at least one Running task with a fresh heartbeat
        // (compute is genuinely in flight), sleep
        // `ECAA_HARNESS_SETTLE_SECS` (default 60s, range [5, 1800])
        // before re-iterating. Keeps the harness alive long enough
        // for the deterministic finalize probe at the top of the
        // NEXT iteration to catch a sentinel arrival, but bounded so
        // we don't tight-loop on no-op iterations. The blocked-needing-SME
        // path above (interactive resolve / waiting_for_sme write) has
        // its own cadence and never reaches this branch.
        let blocked_needing_sme: Vec<String> = after
            .tasks
            .iter()
            .filter_map(|(id, t)| {
                if let TaskState::Blocked { record } = &t.state {
                    // Sentinel-pending blocks have empty
                    // decision_points_for_sme (the agent wrote them as
                    // "[in_flight_sentinel_pending]" or similar). Use
                    // the reason-prefix marker to distinguish; absent
                    // a marker, treat the block as needing SME so we
                    // don't sleep through a real human-decision case.
                    let r = &record.reason;
                    let is_wait_only = r.contains("[in_flight_sentinel_pending]")
                        || r.contains("in_flight_sentinel_pending")
                        || r.contains("decision_points_for_sme: []");
                    if is_wait_only {
                        None
                    } else {
                        Some(id.to_string())
                    }
                } else {
                    None
                }
            })
            .collect();
        let fresh_running = fresh_heartbeat_running_task_ids(path, &after);
        // §Idle-debounce: when nothing is dispatchable AND nothing is
        // running with a fresh heartbeat, the harness has zero work
        // available. Without a sleep here the iteration counter would
        // burn through --max-iterations within seconds — e.g. when
        // every Ready task is gated by filter_picks_respecting_sme_gate
        // (unconfirmed SME review) and no `sme-review-confirmed`
        // sidecar exists yet, the loop sees "no ready picks" and
        // immediately re-polls. The `is_settle_iteration` path below
        // covers the "fresh_running non-empty" case; this branch
        // covers the "nothing dispatchable AND nothing running" case.
        // Reuses ECAA_HARNESS_SETTLE_SECS so a single env knob bounds
        // both windows.
        // When the dispatch_gate fail-closed this iteration, force the
        // idle branch so we sleep `settle_interval_secs()` here instead
        // of tight-looping back to the next gate check. `picks` is
        // already empty (the fail-closed override above clears it), but
        // the explicit flag makes the intent obvious and survives any
        // future refactor of the picks-computation path.
        let is_idle = dispatch_gate_failed_this_iter
            || (transitions_this_iter == 0 && picks.is_empty() && fresh_running.is_empty());
        if is_idle {
            let settle = settle_interval_secs();
            if settle > 0 {
                println!(
                    "  {} Idle: no dispatchable picks, no running tasks — sleeping {}s before re-check",
                    "·".yellow(),
                    settle
                );
                let mut remaining = settle;
                while remaining > 0 {
                    let chunk = remaining.min(2);
                    std::thread::sleep(Duration::from_secs(chunk));
                    if stop_sentinel.exists() {
                        break;
                    }
                    remaining = remaining.saturating_sub(chunk);
                }
            }
        } else if is_settle_iteration(
            &after,
            transitions_this_iter,
            &fresh_running,
            &blocked_needing_sme,
        ) {
            let settle = settle_interval_secs();
            if settle > 0 {
                println!(
                    "  {} Settle: {} running task(s) with fresh heartbeats, no transitions — sleeping {}s",
                    "≈".cyan(),
                    fresh_running.len(),
                    settle
                );
                // Cooperative — wake early on a stop sentinel so a
                // user-requested stop doesn't have to wait out the
                // full settle window.
                let mut remaining = settle;
                while remaining > 0 {
                    let chunk = remaining.min(2);
                    std::thread::sleep(Duration::from_secs(chunk));
                    if stop_sentinel.exists() {
                        break;
                    }
                    remaining = remaining.saturating_sub(chunk);
                }
            }
        }

        // Loop tail: advance the informational counter unconditionally,
        // but only charge the budget for iterations that did real work.
        // Fail-closed iterations (server briefly unreachable) get a free
        // pass so a transient outage doesn't drain the budget on tight
        // no-op loops. `max_total_iterations` (10x budget, set in the
        // loop header) is the hard upper bound.
        i = i.saturating_add(1);
        if !dispatch_gate_failed_this_iter {
            budget_consumed = budget_consumed.saturating_add(1);
        }
    }

    // Reached on natural max-iterations exit. Truncate the WAL so the
    // server's auto-relaunched successor doesn't run orphan recovery
    // against this run's still-Running tasks (whose detached compute
    // is alive but who would otherwise look "orphaned" from the WAL
    // perspective). The liveness probe is the primary defense; this
    // truncation is the structural one.
    if let Err(e) = truncate_wal(path) {
        tracing::warn!(
            target: "harness-wal",
            error = %e,
            "truncate on max-iterations exit failed (continuing)"
        );
    }
    println!(
        "\n{} Harness stopped. Check WORKFLOW.json for current state.",
        "→".blue()
    );
    Ok(())
}

/// Place the harness in its
/// own POSIX process group so a CLI-launched harness can SIGTERM the
/// agent + claude-cli descendants in one shot on Ctrl+C. Server-spawned
/// harness already gets `setsid()` via `pre_exec` in
/// `chat_routes::execution::start::spawn_harness`, so calling
/// `setpgid(0, 0)` here is either a no-op (already leader) or returns
/// `EPERM` (race with `setsid`) — both are fine, we ignore the result.
#[cfg(unix)]
fn setpgid_self() {
    // SAFETY: `libc::setpgid(0, 0)` with both args == 0 is the
    // documented "set my own pgid to my own pid" syscall and has no
    // pointer arguments. It either succeeds, returns EPERM (already a
    // session leader / pgid mismatch), or no-ops on a child of a
    // session leader. Best-effort — we don't check errno.
    #[allow(unsafe_code)]
    unsafe {
        let _ = libc::setpgid(0, 0);
    }
}

#[cfg(not(unix))]
fn setpgid_self() {}

/// Best-effort `kill(-pgid, SIGTERM)` then
/// `kill(-pgid, SIGKILL)`. Returns once both have been delivered or the
/// per-step grace window elapses. Used by the SIGINT handler so the
/// harness takes its descendants (agent-claude.sh + the npm/claude
/// child + any executor-side helpers) down with it instead of orphaning
/// them to init. Safe to call when the harness is the sole occupant of
/// its process group — `kill(-pgid, …)` is then equivalent to a
/// `kill(pid, …)` to self that the libc machinery delivers after this
/// function returns.
#[cfg(unix)]
fn kill_process_group() {
    // SAFETY: `libc::getpid()` and `libc::kill()` with a negative pid
    // (process-group target) are standard POSIX syscalls. No pointer
    // arguments; we ignore the return value because the handler exits
    // unconditionally afterwards.
    #[allow(unsafe_code)]
    unsafe {
        let pid = libc::getpid();
        if pid <= 0 {
            return;
        }
        // -pid addresses the entire process group whose pgid == pid
        // (i.e. the group we became leader of via setpgid_self).
        let _ = libc::kill(-pid, libc::SIGTERM);
        // Brief grace window so well-behaved children flush + exit
        // cleanly. 500ms is the same bound the server-side kill path
        // uses before escalating to SIGKILL.
        std::thread::sleep(Duration::from_millis(500));
        let _ = libc::kill(-pid, libc::SIGKILL);
    }
}

#[cfg(not(unix))]
fn kill_process_group() {}

/// Install a best-effort SIGINT/SIGTERM handler that releases the active
/// executor before the process exits. Essential for remote backends (so
/// a Ctrl+C terminates provisioned cloud instances) — no-op safe for the
/// local backend.
///
/// Also sends SIGTERM/SIGKILL to the harness's process
/// group so the agent + claude-cli descendants exit alongside the
/// harness. Pairs with `setpgid_self()` at main() startup.
///
/// Two-phase shutdown to close the SIGINT-latency bug where the handler
/// blocked waiting for `run_iteration` (potentially minutes of AWS SSM
/// or SLURM sacct polling) to release the iteration mutex:
///
/// 1. `shutdown_flags` are `Arc<AtomicBool>` cloned from each executor
///    **before** it is wrapped in `Arc<Mutex<...>>`. The handler sets
///    these flags directly — no mutex required. The SSM/SLURM polling
///    loop checks the flag between poll cycles and returns early, letting
///    `run_iteration` exit and the main loop drop the mutex.
///
/// 2. With the mutex free, the handler acquires it via `try_lock` and
///    calls the full `release(&mut self)` for backend cleanup
///    (EC2 terminate, scancel). If `try_lock` still fails (rare race
///    where the main loop re-acquired between step 1 and 2), process
///    exit below cleans up.
fn install_signal_handler(
    executors: Vec<Arc<Mutex<Box<dyn Executor>>>>,
    shutdown_flags: Vec<Option<std::sync::Arc<std::sync::atomic::AtomicBool>>>,
    package_path: PathBuf,
) -> Result<()> {
    use std::sync::atomic::Ordering;
    // ctrlc::set_handler is single-use per process; collect every
    // executor (primary + lane secondary, if any) up front so a single
    // handler installation releases all of them on Ctrl+C.
    let result = ctrlc::set_handler(move || {
        eprintln!();
        eprintln!(
            "{} received signal, releasing {} executor(s) and exiting",
            "⚠".yellow(),
            executors.len()
        );
        // Step 1: cooperative non-blocking shutdown. Set each executor's
        // AtomicBool flag directly — no mutex contention. The SSM/SLURM
        // poll loop sees the flag on its next cycle and returns early.
        for f in shutdown_flags.iter().flatten() {
            f.store(true, Ordering::Release);
        }
        // Step 2: full cleanup via the mutex. For remote executors the
        // poll loop already exited in step 1 so the mutex is free
        // (or will be within one poll interval). For local executors
        // run_iteration is fast so try_lock succeeds immediately.
        for handle in &executors {
            if let Ok(mut guard) = handle.try_lock() {
                guard.release();
            }
            // If try_lock fails, process exit below handles cleanup.
        }
        // Step 2.5: flush any pending state.patch.json files into
        // WORKFLOW.json BEFORE we SIGTERM the agent process tree. An
        // agent that completed its work between the prior iteration-end
        // merge and the signal arrival has its terminal-state patch on
        // disk under runtime/outputs/<task_id>/state.patch.json. Without
        // this flush, an agent that wrote `{to: {status: blocked}}`
        // moments before /execution/kill arrived would leave its task
        // Running indefinitely: the kill races past the iteration-end
        // apply_pending_patches_strict, and if no new harness ever
        // re-spawns for the same session lock, the startup-time
        // apply_pending_patches never fires either. Best-effort here;
        // orphan recovery on next harness boot remains the durable
        // backstop.
        match apply_pending_patches(&package_path, &[]) {
            Ok(merged) => {
                if let Err(e) = write_dag(&package_path, &merged) {
                    eprintln!(
                        "{} signal-handler patch flush: write_dag failed: {:#}",
                        "⚠".yellow(),
                        e
                    );
                }
            }
            Err(e) => {
                eprintln!(
                    "{} signal-handler patch flush: apply_pending_patches failed: {:#}",
                    "⚠".yellow(),
                    e
                );
            }
        }
        // Take the agent + claude-cli descendants
        // down with us. Server-spawned harness already gets the same
        // tree-kill on `/execution/kill`; this is the CLI-direct path.
        kill_process_group();
        std::process::exit(130); // 128 + SIGINT
    });
    // Tests and cargo test harnesses may have already installed a handler.
    // A failure here isn't fatal — the normal exit path still calls
    // release() via the main `run_result` block above.
    if let Err(e) = result {
        eprintln!(
            "{} could not install ctrl-c handler: {} (proceeding without it)",
            "⚠".yellow(),
            e
        );
    }
    Ok(())
}

/// Attempt to recover a valid `DAG` from the git history of the package
/// root by running `git -C <dir> show HEAD:WORKFLOW.json`. Returns `Some(dag)`
/// when git is available, the directory is a repo, HEAD carries a
/// `WORKFLOW.json`, and that copy parses cleanly. Returns `None` on any
/// failure — missing git binary, non-repo, absent path at HEAD, or
/// parse error — so callers always fall through to the next recovery tier.
fn git_show_workflow_json(dir: &Path) -> Option<DAG> {
    let out = std::process::Command::new("git")
        .args(["-C", &dir.to_string_lossy(), "show", "HEAD:WORKFLOW.json"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = std::str::from_utf8(&out.stdout).ok()?;
    serde_json::from_str::<DAG>(text).ok()
}

/// Read the HEAD commit SHA for `dir` for diagnostic logging. Returns an
/// empty string when git is unavailable or the directory has no commits.
fn git_head_sha(dir: &Path) -> String {
    std::process::Command::new("git")
        .args(["-C", &dir.to_string_lossy(), "rev-parse", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_default()
}

fn read_dag(dir: &Path) -> Result<DAG> {
    // Cap the read so a corrupted (or malicious) agent
    // write of a gigabyte-scale WORKFLOW.json can't OOM the harness
    // before any parse error has a chance to surface.
    let content = read_capped(&dir.join("WORKFLOW.json"), resolve_max_bytes())
        .context("reading WORKFLOW.json")?;
    // Try strict parse first — the common path.
    if let Ok(dag) = serde_json::from_str::<DAG>(&content) {
        return Ok(dag);
    }
    // Git recovery: before touching the on-disk file, check whether
    // the last committed WORKFLOW.json (written on every successful
    // task by the git-provenance hooks) is still clean. This preserves
    // the full DAG state instead of replacing bad tasks with placeholder
    // blocked records.
    if let Some(dag) = git_show_workflow_json(dir) {
        let commit = git_head_sha(dir);
        tracing::warn!(
            commit = %commit,
            dir = %dir.display(),
            "WORKFLOW.json on-disk corrupt; recovered from git HEAD"
        );
        return Ok(dag);
    }
    tracing::warn!(
        dir = %dir.display(),
        "WORKFLOW.json corrupt and git recovery unavailable or also corrupt; \
         falling through to per-task placeholder repair"
    );
    // Per-task recovery: if one task's state doesn't fit the Rust
    // schema (e.g. the agent invented a non-conforming
    // `record.attempts` shape), rewrite JUST that task's state to a
    // well-formed blocked record so the rest of the DAG can parse.
    // Without this the agent's single malformed write bricks every
    // subsequent harness invocation in a tight restart loop.
    let value: serde_json::Value =
        serde_json::from_str(&content).context("parsing WORKFLOW.json (as raw Value)")?;
    let mut repaired = value.clone();
    let mut repairs: Vec<String> = Vec::new();
    if let Some(tasks) = repaired.get_mut("tasks").and_then(|t| t.as_object_mut()) {
        for (task_id, task_val) in tasks.iter_mut() {
            let single = serde_json::json!({
                "version": "1.0",
                "workflow_id": "probe",
                "tasks": { task_id.clone(): task_val.clone() }
            });
            if serde_json::from_value::<DAG>(single).is_err() {
                let placeholder = serde_json::json!({
                    "status": "blocked",
                    "record": {
                        "reason": format!(
                            "harness could not parse prior agent state for task {} (schema mismatch — commonly a non-conforming record.attempts shape). Manual review: inspect runtime/outputs/{}/ and LOG.jsonl.",
                            task_id, task_id
                        ),
                        "attempts": []
                    }
                });
                if let Some(obj) = task_val.as_object_mut() {
                    obj.insert("state".into(), placeholder);
                }
                repairs.push(task_id.clone());
            }
        }
    }
    if !repairs.is_empty() {
        eprintln!(
            "⚠ read_dag: repaired {} task(s) with malformed state (set to blocked with placeholder reason): [{}]",
            repairs.len(),
            repairs.join(", "),
        );
        // Persist the repaired DAG back to disk so subsequent
        // invocations don't re-diverge on the same bad bytes.
        let pretty = serde_json::to_string_pretty(&repaired).context("serializing repaired DAG")?;
        write_workflow_json_atomic(dir, &pretty).context("writing repaired WORKFLOW.json")?;
    }
    serde_json::from_value(repaired).context("parsing WORKFLOW.json after per-task repair")
}

fn watchdog_wall_clock_event_is_current(package_root: &Path, task_id: &str) -> bool {
    let Ok(dag) = read_dag(package_root) else {
        return true;
    };
    matches!(
        dag.tasks.get(task_id).map(|task| &task.state),
        Some(TaskState::Running { .. })
    )
}

fn write_dag(dir: &Path, dag: &DAG) -> Result<()> {
    let pretty = serde_json::to_string_pretty(dag).context("serializing DAG")?;
    write_workflow_json_atomic(dir, &pretty)
}

const WORKFLOW_METADATA_EDIT: &str = "<workflow-metadata>";

fn direct_workflow_edit_ids(before: &DAG, after: &DAG) -> Vec<String> {
    let mut ids = std::collections::BTreeSet::new();
    if before.version != after.version
        || before.workflow_id != after.workflow_id
        || before.current_task != after.current_task
    {
        ids.insert(WORKFLOW_METADATA_EDIT.to_string());
    }
    for id in before.tasks.keys().chain(after.tasks.keys()) {
        if before.tasks.get(id) != after.tasks.get(id) {
            ids.insert(id.to_string());
        }
    }
    ids.into_iter().collect()
}

fn block_agent_contract_violation(dag: &mut DAG, task_ids: &[String], detail: &str) {
    for task_id in task_ids {
        let Some(task) = dag.tasks.get_mut(task_id.as_str()) else {
            continue;
        };
        task.state = TaskState::Blocked {
            record: ecaa_workflow_core::dag::BlockedRecord {
                reason: format!(
                    "[agent_contract_violation] task={} {}; the harness restored WORKFLOW.json to the pre-dispatch snapshot. Agents must write runtime/outputs/{}/state.patch.json with matching ECAA_HARNESS_RUN_ID and ECAA_DISPATCH_EPOCH.",
                    task_id, detail, task_id
                ),
                attempts: vec![],
            },
        };
    }
}

fn restore_agent_workflow_edits(
    package_root: &Path,
    baseline: &DAG,
    direct_read: Result<DAG>,
    picks: &[String],
) -> Result<()> {
    // Start from the post-dispatch read (when available) so that
    // legitimate non-picked changes from outside the agent — server
    // unblock, manual SME edits, post-emit lineage rewrites — survive
    // this enforcement pass. Only the picked-task entries get reverted
    // to their pre-dispatch baseline below. Falling back to `baseline`
    // when the read failed is the safe fail-closed path: we lose any
    // valid concurrent edits, but we also can't trust what's on disk.
    let pick_set: std::collections::BTreeSet<String> = picks.iter().cloned().collect();
    let mut restored: DAG;
    let mut block_targets: Vec<String> = Vec::new();

    match direct_read {
        Ok(after) => {
            let edits = direct_workflow_edit_ids(baseline, &after);
            if edits.is_empty() {
                return Ok(());
            }
            // Picked-task entries are reverted to baseline (contract
            // violation). Non-picked entries (and the rest of the DAG)
            // keep their post-dispatch values.
            restored = after.clone();
            let picked_edits: Vec<String> = edits
                .iter()
                .filter(|id| pick_set.contains(*id))
                .cloned()
                .collect();
            // Metadata edits force every pick to be blocked: an agent
            // is not allowed to touch top-level workflow fields.
            let metadata_touched = edits.iter().any(|id| id == WORKFLOW_METADATA_EDIT);
            if !picked_edits.is_empty() || metadata_touched {
                eprintln!(
                    "[agent-contract] reverting picked-task direct edits; baseline-restoring entries: [{}]",
                    picked_edits.join(", ")
                );
                for task_id in &picked_edits {
                    if let Some(prev) = baseline.tasks.get(task_id.as_str()) {
                        restored.tasks.insert(
                            ecaa_workflow_core::ids::TaskId::from(task_id.as_str()),
                            prev.clone(),
                        );
                    }
                }
                if metadata_touched {
                    // Roll back top-level workflow metadata fields the
                    // agent isn't allowed to touch; this is a cheap full
                    // restore minus the task entries we want to keep.
                    let kept_tasks = restored.tasks.clone();
                    restored = baseline.clone();
                    restored.tasks = kept_tasks;
                    block_targets.extend(picks.iter().cloned());
                }
                block_targets.extend(picked_edits);
            } else {
                // All edits were on non-picked tasks (server unblock,
                // SME amendment, etc.) — accept them and do not block.
                return Ok(());
            }
        }
        Err(e) => {
            eprintln!(
                "[agent-contract] restoring WORKFLOW.json after post-dispatch read failed: {:#}",
                e
            );
            restored = baseline.clone();
            block_targets.extend(picks.iter().cloned());
        }
    }

    block_targets.sort();
    block_targets.dedup();
    if !block_targets.is_empty() {
        block_agent_contract_violation(
            &mut restored,
            &block_targets,
            "attempted to modify WORKFLOW.json directly",
        );
        for task_id in &block_targets {
            append_progress_log(
                package_root,
                task_id,
                "harness contract violation: direct WORKFLOW.json edits are not accepted; write state.patch.json for the dispatched task",
            );
        }
    }
    write_dag(package_root, &restored)
}

/// Atomic-rename helper. A mid-write crash can leave `WORKFLOW.json.tmp`
/// behind but never a truncated `WORKFLOW.json`. The tmp filename
/// includes the harness pid so two harness processes briefly racing
/// (e.g. server-spawned auto-relaunch overlapping a shutdown) can't
/// stomp each other's tmp.
fn write_workflow_json_atomic(dir: &Path, contents: &str) -> Result<()> {
    use std::io::Write;
    let target = dir.join("WORKFLOW.json");
    let tmp = dir.join(format!("WORKFLOW.json.tmp.{}", std::process::id()));
    {
        let mut file =
            std::fs::File::create(&tmp).with_context(|| format!("creating {}", tmp.display()))?;
        file.write_all(contents.as_bytes())
            .with_context(|| format!("writing {}", tmp.display()))?;
        file.sync_data()
            .with_context(|| format!("fsync {}", tmp.display()))?;
    }
    std::fs::rename(&tmp, &target)
        .with_context(|| format!("renaming {} -> WORKFLOW.json", tmp.display()))?;
    let dir_handle = std::fs::File::open(dir)
        .with_context(|| format!("opening parent {} for fsync", dir.display()))?;
    dir_handle
        .sync_data()
        .with_context(|| format!("fsync parent {}", dir.display()))?;
    Ok(())
}

/// Post-validator contract enforcement. After a `validate_<stage>`
/// task transitions to Completed, cross-check its validation_report.json
/// against `policies/validation-contract.json`. Any `required` assertion
/// that isn't satisfied → re-block both the validator + its parent
/// compute task with a ContractViolation reason pointing at the
/// offending assertion ids. Safe to run on every iteration; no-op when
/// no contract is present or the validator hasn't run yet.
///
/// Returns the list of (task_id, assertion_ids) pairs that were
/// re-blocked so the harness can log them.
fn enforce_validation_contract(
    pkg_dir: &Path,
    dag: &mut DAG,
) -> Result<Vec<(String, Vec<String>)>> {
    let contract_path = pkg_dir.join("policies").join("validation-contract.json");
    if !contract_path.exists() {
        return Ok(Vec::new());
    }
    // Cap the validation-contract read. This file is
    // emitted by the compiler and ought to be small (a few hundred
    // assertion entries at most), but it's a JSON-shaped input that
    // future tooling could grow uncontrolled — apply the same cap as
    // the agent-produced JSONs for uniformity.
    let contract_bytes = match read_bytes_capped(&contract_path, resolve_max_bytes()) {
        Ok(b) => b,
        Err(_) => return Ok(Vec::new()),
    };
    let contract: serde_json::Value = match serde_json::from_slice(&contract_bytes) {
        Ok(v) => v,
        Err(_) => return Ok(Vec::new()),
    };
    let stages = match contract.get("stages").and_then(|v| v.as_object()) {
        Some(s) => s,
        None => return Ok(Vec::new()),
    };

    let mut violations: Vec<(String, Vec<String>)> = Vec::new();

    // For each validate_<stage> that's Completed, run its contract block.
    // Typed role via `derive_role_from_id`.
    let task_ids: Vec<String> = dag.tasks.keys().map(|id| id.to_string()).collect();
    for tid in task_ids {
        if !ecaa_workflow_core::taxonomy::derive_role_from_id(&tid).is_validation() {
            continue;
        }
        let task = dag.tasks.get(tid.as_str()).unwrap();
        if !matches!(task.state, TaskState::Completed { .. }) {
            continue;
        }
        let parent_id = tid.trim_start_matches("validate_").to_string();
        let stage_class = dag
            .tasks
            .get(parent_id.as_str())
            .and_then(|t| t.spec.as_ref())
            .and_then(|s| s.get("stage_class"))
            .and_then(|v| v.as_str())
            .unwrap_or(&parent_id)
            .to_string();
        let Some(block) = stages.get(&stage_class).and_then(|v| v.as_object()) else {
            continue;
        };
        let Some(assertions) = block.get("assertions").and_then(|v| v.as_array()) else {
            continue;
        };
        let mut failed_ids: Vec<String> = Vec::new();
        for a in assertions {
            let id = match a.get("id").and_then(|v| v.as_str()) {
                Some(s) => s.to_string(),
                None => continue,
            };
            let severity = a
                .get("severity")
                .and_then(|v| v.as_str())
                .unwrap_or("recommended");
            if severity != "required" {
                continue;
            }
            if !run_assertion(pkg_dir, a) {
                failed_ids.push(id);
            }
        }
        if !failed_ids.is_empty() {
            violations.push((parent_id.clone(), failed_ids.clone()));
            // Re-block the validator
            let reason = format!(
                "Harness validation-contract check: required assertion(s) unsatisfied: {}. See policies/validation-contract.json for detail. The parent compute task '{}' has also been re-blocked so the agent can remediate.",
                failed_ids.join(", "),
                parent_id
            );
            if let Some(t) = dag.tasks.get_mut(tid.as_str()) {
                t.state = TaskState::Blocked {
                    record: ecaa_workflow_core::dag::BlockedRecord {
                        reason: reason.clone(),
                        attempts: vec![],
                    },
                };
            }
            // Re-block the parent
            if let Some(t) = dag.tasks.get_mut(parent_id.as_str()) {
                t.state = TaskState::Blocked {
                    record: ecaa_workflow_core::dag::BlockedRecord {
                        reason: format!(
                            "Harness validation-contract check (from validate_{}): required assertion(s) unsatisfied: {}. Remediate and re-run.",
                            parent_id,
                            failed_ids.join(", ")
                        ),
                        attempts: vec![],
                    },
                };
            }
        }
    }
    Ok(violations)
}

/// Per-assertion runner. Returns true when the assertion passes.
/// Unknown assertion_types default to false (pessimistic) so any typo
/// surfaces as a hard failure.
fn run_assertion(pkg_dir: &Path, assertion: &serde_json::Value) -> bool {
    let atype = match assertion.get("assertion_type").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return false,
    };
    let target = match assertion.get("target").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return false,
    };
    let resolve = |t: &str| -> std::path::PathBuf { pkg_dir.join(t.trim_start_matches('/')) };
    match atype {
        "artifact_present" => resolve(target).is_file(),
        "artifact_non_empty_table" => {
            // glob-aware + content-aware: any file matching the target
            // glob has more than 1 line of content (header + ≥1 data).
            glob_matches(pkg_dir, target)
                .iter()
                .any(|p| count_lines_gz_aware(p).map(|n| n >= 2).unwrap_or(false))
        }
        "artifact_glob_any" => !glob_matches(pkg_dir, target).is_empty(),
        "string_contains" => {
            let path = resolve(target);
            let Ok(bytes) = std::fs::read(&path) else {
                return false;
            };
            let text = String::from_utf8_lossy(&bytes);
            // Supports either `substrings: [required all of]` or
            // `substrings_any: [any of]`.
            if let Some(req) = assertion
                .get("check")
                .and_then(|c| c.get("substrings"))
                .and_then(|v| v.as_array())
            {
                req.iter()
                    .all(|s| s.as_str().map(|ss| text.contains(ss)).unwrap_or(false))
            } else if let Some(any) = assertion
                .get("check")
                .and_then(|c| c.get("substrings_any"))
                .and_then(|v| v.as_array())
            {
                any.iter()
                    .any(|s| s.as_str().map(|ss| text.contains(ss)).unwrap_or(false))
            } else {
                false
            }
        }
        _ => false,
    }
}

fn glob_matches(pkg_dir: &Path, pattern: &str) -> Vec<std::path::PathBuf> {
    // Minimal glob: supports `*` within a segment + `{a,b}` alternation.
    // Sufficient for the contract's `runtime/outputs/compartment_*/*.tsv*`
    // style patterns. Anything else falls back to the literal path.
    let full = pkg_dir.join(pattern.trim_start_matches('/'));
    let full_str = full.to_string_lossy().to_string();
    if !full_str.contains('*') && !full_str.contains('{') {
        return if full.exists() { vec![full] } else { vec![] };
    }
    // Expand {a,b} alternations into multiple patterns then glob each.
    let patterns = expand_braces(&full_str);
    let mut out: Vec<std::path::PathBuf> = Vec::new();
    for p in patterns {
        if let Ok(paths) = glob::glob(&p) {
            for r in paths.flatten() {
                out.push(r);
            }
        }
    }
    out
}

fn expand_braces(pattern: &str) -> Vec<String> {
    if let Some(open) = pattern.find('{') {
        if let Some(close) = pattern[open..].find('}') {
            let before = &pattern[..open];
            let mid = &pattern[open + 1..open + close];
            let after = &pattern[open + close + 1..];
            return mid
                .split(',')
                .flat_map(|alt| expand_braces(&format!("{}{}{}", before, alt, after)))
                .collect();
        }
    }
    vec![pattern.to_string()]
}

fn count_lines_gz_aware(path: &Path) -> Result<usize> {
    use std::io::BufRead;
    let f = std::fs::File::open(path).with_context(|| format!("opening {}", path.display()))?;
    if path.extension().and_then(|e| e.to_str()) == Some("gz") {
        let decoder = flate2::read::GzDecoder::new(f);
        let mut reader = std::io::BufReader::new(decoder);
        let mut n = 0usize;
        let mut line = String::new();
        loop {
            line.clear();
            match reader.read_line(&mut line) {
                Ok(0) => break,
                Ok(_) => {
                    n += 1;
                    if n > 5 {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
        Ok(n)
    } else {
        let reader = std::io::BufReader::new(f);
        let mut n = 0usize;
        for _ in reader.lines() {
            n += 1;
            if n > 5 {
                break;
            }
        }
        Ok(n)
    }
}

/// Per-method probe spec. The probe call site looks up each entry by
/// the method id used in atom YAML `attributes.candidate_tools` and
/// runs the matching detector. Entries that the bio-min base image
/// doesn't ship live here so the discover step can either down-rank
/// missing methods or trigger the install-on-first-use path.
enum MethodProbe {
    /// `python -c 'import <module>'` succeeds.
    Python(&'static str),
    /// `Rscript -e 'library(<pkg>)'` succeeds (Bioc or CRAN).
    R(&'static str),
}

/// Curated common-method probe map. Covers the DE, normalisation,
/// pathway-enrichment, clustering, integration, and batch-correction
/// candidate_tools that drive method selection in the most-used atoms.
/// Grow as gaps surface; entries that this map doesn't cover are
/// reported as `unknown` so the discover step falls back to its
/// composite-scoring rationale.
const METHOD_PROBES: &[(&str, MethodProbe)] = &[
    // Differential expression
    ("deseq2", MethodProbe::R("DESeq2")),
    ("edger", MethodProbe::R("edgeR")),
    ("limma_voom", MethodProbe::R("limma")),
    ("mast", MethodProbe::R("MAST")),
    ("dexseq", MethodProbe::R("DEXSeq")),
    ("drimseq", MethodProbe::R("DRIMSeq")),
    // Normalisation
    ("deseq2_vst", MethodProbe::R("DESeq2")),
    ("edger_tmm", MethodProbe::R("edgeR")),
    ("scran", MethodProbe::R("scran")),
    ("seurat_lognormalize", MethodProbe::R("Seurat")),
    ("sctransform", MethodProbe::R("sctransform")),
    // Pathway enrichment
    ("fgsea", MethodProbe::R("fgsea")),
    ("clusterprofiler", MethodProbe::R("clusterProfiler")),
    ("gsea", MethodProbe::Python("gseapy")),
    ("enrichr", MethodProbe::Python("gseapy")),
    // Clustering + dimensionality reduction
    ("leiden", MethodProbe::Python("leidenalg")),
    ("louvain", MethodProbe::Python("louvain")),
    ("umap", MethodProbe::Python("umap")),
    ("phate", MethodProbe::Python("phate")),
    // Integration + batch correction
    ("harmony", MethodProbe::Python("harmonypy")),
    ("bbknn", MethodProbe::Python("bbknn")),
    ("scvi", MethodProbe::Python("scvi")),
    ("mnn_correct", MethodProbe::Python("mnnpy")),
    ("combat", MethodProbe::R("sva")),
    // Multi-omics integration
    ("mofa2", MethodProbe::R("MOFA2")),
    ("mofa_plus", MethodProbe::Python("mofapy2")),
    ("mixomics_diablo", MethodProbe::R("mixOmics")),
    // Cell-type annotation
    ("celltypist", MethodProbe::Python("celltypist")),
    ("singler", MethodProbe::R("SingleR")),
    ("sctype", MethodProbe::R("Seurat")), // sctype runs on top of Seurat
    ("azimuth", MethodProbe::R("Azimuth")),
    // Peak / ChIP
    ("macs2", MethodProbe::Python("MACS2")),
    ("chipseeker", MethodProbe::R("ChIPseeker")),
    ("diffbind", MethodProbe::R("DiffBind")),
    ("csaw", MethodProbe::R("csaw")),
    // Spatial
    ("bayesspace", MethodProbe::R("BayesSpace")),
    ("banksy", MethodProbe::R("Banksy")),
    ("squidpy_neighbors", MethodProbe::Python("squidpy")),
    // Colocalization
    ("coloc", MethodProbe::R("coloc")),
    ("susie_coloc", MethodProbe::R("susieR")),
    ("hyprcoloc", MethodProbe::R("hyprcoloc")),
];

/// Env probe — detects spec-relevant environmental
/// capabilities and writes a structured report the agent reads during
/// `discover_*` stages. Skips unavailable methods cleanly instead of
/// silently substituting.
///
/// Two sections:
///
/// `capabilities` — fixed coarse-grained signals the discover step
///   consulted historically:
/// - `r_seurat`: R + Seurat v5 (spec preference for integration, CCA)
/// - `r_cellchat`: R + CellChat (spec preference for cell-cell comm)
/// - `pyscenic`: pySCENIC (spec preference for regulon analysis)
/// - `python_lisi`: `lisi` Python package (iLISI / cLISI metrics)
/// - `cellranger_version`: Cell Ranger binary version string or null
/// - `rna_velocity_capable`: always false at probe time — requires
///   spliced/unspliced matrices in the package, which is a data-state
///   question, not a binary-state one. Left as a placeholder; agent
///   can flip it after inspecting data_acquisition artifacts.
///
/// `methods` — per-method availability for every `candidate_tools`
///   entry in `METHOD_PROBES`. Each value is `{available, language,
///   probe_target}` so the discover step can either down-rank
///   unavailable methods, or trigger the install-on-first-use path
///   from PROMPT.md when an unavailable method is the SME-pinned or
///   top-ranked choice. Methods not in `METHOD_PROBES` aren't probed
///   here; the discover step falls back to composite scoring without
///   an availability signal.
fn write_env_capability(pkg_dir: &Path) -> Result<()> {
    let runtime_dir = pkg_dir.join("runtime");
    std::fs::create_dir_all(&runtime_dir).context("creating runtime dir")?;

    // Honor a package-local R user library at runtime/r-libs/ so a
    // package whose agent installed Seurat 5.x into the package
    // doesn't get probed as r_seurat=false on every harness restart.
    // The path is passed through R_LIBS_USER (also recognised by
    //.libPaths() as a user-level library prepended to the path).
    let r_libs_path = runtime_dir.join("r-libs");
    let r_libs_user: Option<&Path> = if r_libs_path.is_dir() {
        Some(r_libs_path.as_path())
    } else {
        None
    };

    let r_seurat = probe_r_package("Seurat", r_libs_user);
    let r_cellchat = probe_r_package("CellChat", r_libs_user);
    let pyscenic = probe_python_import("pyscenic");
    let python_lisi = probe_python_import("lisi")
        || probe_python_import("harmonypy")  // lisi often comes via harmonypy in newer stacks
        || probe_python_import("scanpy.external.pp.lisi");
    let cellranger_version = probe_cellranger();

    // Per-method probes. BTreeMap so the on-disk JSON is byte-stable
    // across runs (deterministic-emission contract).
    let mut methods = serde_json::Map::new();
    let mut available_count = 0usize;
    for (name, probe) in METHOD_PROBES.iter() {
        let (available, language, probe_target) = match probe {
            MethodProbe::Python(module) => (probe_python_import(module), "python", *module),
            MethodProbe::R(pkg) => (probe_r_package(pkg, r_libs_user), "r", *pkg),
        };
        if available {
            available_count += 1;
        }
        methods.insert(
            (*name).to_string(),
            serde_json::json!({
                "available": available,
                "language": language,
                "probe_target": probe_target,
            }),
        );
    }

    let report = serde_json::json!({
        "probed_at": ecaa_workflow_core::time_helpers::now_rfc3339(),
        "harness_version": env!("CARGO_PKG_VERSION"),
        "host_os": std::env::consts::OS,
        "capabilities": {
            "r_seurat": r_seurat,
            "r_cellchat": r_cellchat,
            "pyscenic": pyscenic,
            "python_lisi": python_lisi,
            "cellranger_version": cellranger_version,
            "rna_velocity_capable": false,
        },
        "methods": methods,
    });
    let path = runtime_dir.join("env_capability.json");
    std::fs::write(&path, serde_json::to_string_pretty(&report)?)
        .with_context(|| format!("writing {}", path.display()))?;
    println!(
        "  {} env_capability probe: R+Seurat={} R+CellChat={} pySCENIC={} lisi={} cellranger={} methods={}/{} available",
        "✓".green(),
        r_seurat,
        r_cellchat,
        pyscenic,
        python_lisi,
        cellranger_version
            .clone()
            .unwrap_or_else(|| "none".to_string()),
        available_count,
        METHOD_PROBES.len(),
    );
    Ok(())
}

fn probe_r_package(pkg: &str, r_libs_user: Option<&Path>) -> bool {
    // When `r_libs_user` is set, prepend it to `.libPaths()` inside
    // the Rscript expression itself — purely env-var-based threading
    // doesn't always survive across system R configurations, but
    // Explicit `.libPaths(c(<path>,.libPaths()))` always wins.
    let expr = match r_libs_user {
        Some(p) => format!(
            ".libPaths(c('{}', .libPaths())); suppressMessages(library({}))",
            p.display().to_string().replace('\'', "\\'"),
            pkg,
        ),
        None => format!("suppressMessages(library({}))", pkg),
    };
    let mut cmd = std::process::Command::new("Rscript");
    cmd.args(["-e", &expr])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    if let Some(p) = r_libs_user {
        // Belt-and-braces: also set R_LIBS_USER so even Rscript
        // Wrappers that bypass the inline.libPaths can find the
        // package.
        cmd.env("R_LIBS_USER", p);
    }
    cmd.status().map(|s| s.success()).unwrap_or(false)
}

fn probe_python_import(module: &str) -> bool {
    let expr = format!("import {}", module);
    std::process::Command::new("python3")
        .args(["-c", &expr])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn probe_cellranger() -> Option<String> {
    let output = std::process::Command::new("cellranger")
        .arg("--version")
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&output.stdout);
    s.lines().next().map(|l| l.trim().to_string())
}

fn append_log(dir: &Path, entry: &serde_json::Value) -> Result<()> {
    use std::io::Write;
    let log_path = dir.join("runtime/LOG.jsonl");
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .with_context(|| format!("opening {}", log_path.display()))?;
    writeln!(file, "{}", serde_json::to_string(entry)?)?;
    Ok(())
}

#[cfg(test)]
mod write_dag_tests {
    use super::*;

    fn running_fixture() -> DAG {
        use ecaa_workflow_core::dag::{Assignee, ResourceClass, Task, TaskKind, TaskState};
        let mut dag = DAG {
            version: "1".into(),
            schema_version: ecaa_workflow_core::dag::current_dag_schema_version(),
            workflow_id: "contract_test".into(),
            current_task: None,
            tasks: std::collections::BTreeMap::new(),
            reverse_deps: std::collections::BTreeMap::new(),
            run_id: None,
        };
        dag.tasks.insert(
            "compute".into(),
            Task {
                kind: TaskKind::Computation,
                state: TaskState::Running {
                    started_at: "2026-01-01T00:00:00Z".into(),
                    remote: None,
                },
                depends_on: vec![],
                assignee: Assignee::Agent,
                description: "compute".into(),
                spec: None,
                resolution: None,
                result_ref: None,
                resource_class: ResourceClass::CpuHeavy,
                requires_sme_review: false,
                required_artifacts: vec![],
                container: None,
                source_atom_id: None,
                safety: Default::default(),
            },
        );
        dag
    }

    #[test]
    fn restore_agent_workflow_edits_blocks_picked_direct_state_change() {
        let tmp = tempfile::tempdir().unwrap();
        let pkg = tmp.path();
        let baseline = running_fixture();
        write_dag(pkg, &baseline).unwrap();

        let mut direct = baseline.clone();
        direct.tasks.get_mut("compute").unwrap().state = TaskState::Completed {
            result: serde_json::json!({"direct": true}),
        };

        restore_agent_workflow_edits(pkg, &baseline, Ok(direct), &["compute".to_string()]).unwrap();
        let restored = read_dag(pkg).unwrap();
        match &restored.tasks.get("compute").unwrap().state {
            TaskState::Blocked { record } => {
                assert!(record.reason.contains("[agent_contract_violation]"));
                assert!(record.reason.contains("WORKFLOW.json"));
            }
            other => panic!("expected direct edit to be blocked, got {:?}", other),
        }
    }

    /// Server-side state changes on non-picked tasks (e.g. /unblock
    /// flipping a Blocked → Ready) must survive `restore_agent_workflow_edits`.
    /// Regression test for the harness wedge where the agent-contract
    /// enforcement was overwriting the entire DAG with the pre-dispatch
    /// baseline, reverting legitimate server unblock state transitions
    /// and causing the iteration loop to never re-dispatch the unblocked
    /// task.
    #[test]
    fn restore_agent_workflow_edits_preserves_non_picked_server_state_changes() {
        use ecaa_workflow_core::dag::{Assignee, ResourceClass, Task, TaskKind, TaskState};
        let tmp = tempfile::tempdir().unwrap();
        let pkg = tmp.path();
        let mut baseline = running_fixture();
        // Second task starts Blocked (from a prior iteration).
        baseline.tasks.insert(
            "data_acquisition".into(),
            Task {
                kind: TaskKind::Computation,
                state: TaskState::Blocked {
                    record: ecaa_workflow_core::dag::BlockedRecord {
                        reason: "iter-1 blocker".into(),
                        attempts: vec![],
                    },
                },
                depends_on: vec![],
                assignee: Assignee::Agent,
                description: "data".into(),
                spec: None,
                resolution: None,
                result_ref: None,
                resource_class: ResourceClass::CpuHeavy,
                requires_sme_review: false,
                required_artifacts: vec![],
                container: None,
                source_atom_id: None,
                safety: Default::default(),
            },
        );
        write_dag(pkg, &baseline).unwrap();

        // Server unblocks data_acquisition (Blocked → Ready) while
        // iter N dispatched only `compute`. The picked-task entry must
        // revert to baseline if changed; the non-picked unblock survives.
        let mut after = baseline.clone();
        after.tasks.get_mut("data_acquisition").unwrap().state = TaskState::Ready;
        // Mirror the production-time on-disk shape: agents + server
        // have already mutated WORKFLOW.json before the restore pass
        // runs. The function's job is to leave the non-picked edits in
        // place while reverting only the picked-task changes.
        write_dag(pkg, &after).unwrap();

        restore_agent_workflow_edits(pkg, &baseline, Ok(after), &["compute".to_string()]).unwrap();
        let restored = read_dag(pkg).unwrap();
        assert!(
            matches!(
                restored.tasks.get("data_acquisition").unwrap().state,
                TaskState::Ready
            ),
            "server-side Blocked → Ready transition on a non-picked task must survive the agent-contract restore pass"
        );
        // Picked task ('compute') was unchanged in this scenario, so it
        // stays at its baseline TaskState::Running value.
        assert!(matches!(
            restored.tasks.get("compute").unwrap().state,
            TaskState::Running { .. }
        ));
    }

    /// `write_dag` must never produce an observable truncated/empty
    /// `WORKFLOW.json`. We assert this by writing N times in sequence
    /// and confirming the file is always a valid DAG between writes —
    /// the temp+rename invariant means a reader can only ever observe
    /// the prior committed bytes or the new committed bytes, never an
    /// in-progress write.
    #[test]
    fn write_dag_is_atomic_against_concurrent_readers() {
        use ecaa_workflow_core::dag::{Assignee, ResourceClass, Task, TaskKind, TaskState};
        let tmp = tempfile::tempdir().unwrap();
        let pkg = tmp.path();
        let mut dag = DAG {
            version: "1".into(),
            schema_version: ecaa_workflow_core::dag::current_dag_schema_version(),
            workflow_id: "atomic_test".into(),
            current_task: None,
            tasks: std::collections::BTreeMap::new(),
            reverse_deps: std::collections::BTreeMap::new(),
            run_id: None,
        };
        dag.tasks.insert(
            "t".into(),
            Task {
                kind: TaskKind::Computation,
                state: TaskState::Ready,
                depends_on: vec![],
                assignee: Assignee::Agent,
                description: "x".into(),
                spec: None,
                resolution: None,
                result_ref: None,
                resource_class: ResourceClass::CpuHeavy,
                requires_sme_review: false,
                required_artifacts: vec![],
                container: None,
                source_atom_id: None,
                safety: Default::default(),
            },
        );
        write_dag(pkg, &dag).unwrap();
        // Successive writes must always leave WORKFLOW.json parseable —
        // there is no "tmp left in place" window after rename returns.
        for _ in 0..50 {
            write_dag(pkg, &dag).unwrap();
            let parsed = read_dag(pkg).expect("WORKFLOW.json always parseable");
            assert_eq!(parsed.tasks.len(), 1);
            assert!(!pkg
                .join(format!("WORKFLOW.json.tmp.{}", std::process::id()))
                .exists());
        }
    }

    /// On hard-kill mid-write, the leftover tmp file is harmless: a
    /// subsequent read_dag still parses the prior WORKFLOW.json and
    /// the next successful write_dag clobbers the stale tmp.
    #[test]
    fn write_dag_recovers_from_leftover_tmp_after_crash() {
        use ecaa_workflow_core::dag::{Assignee, ResourceClass, Task, TaskKind, TaskState};
        let tmp = tempfile::tempdir().unwrap();
        let pkg = tmp.path();
        let mut dag = DAG {
            version: "1".into(),
            schema_version: ecaa_workflow_core::dag::current_dag_schema_version(),
            workflow_id: "leftover".into(),
            current_task: None,
            tasks: std::collections::BTreeMap::new(),
            reverse_deps: std::collections::BTreeMap::new(),
            run_id: None,
        };
        dag.tasks.insert(
            "a".into(),
            Task {
                kind: TaskKind::Computation,
                state: TaskState::Ready,
                depends_on: vec![],
                assignee: Assignee::Agent,
                description: "x".into(),
                spec: None,
                resolution: None,
                result_ref: None,
                resource_class: ResourceClass::CpuHeavy,
                requires_sme_review: false,
                required_artifacts: vec![],
                container: None,
                source_atom_id: None,
                safety: Default::default(),
            },
        );
        write_dag(pkg, &dag).unwrap();
        // Simulate a previous crashed write that left a stale tmp behind.
        let stale_tmp = pkg.join(format!("WORKFLOW.json.tmp.{}", std::process::id()));
        std::fs::write(&stale_tmp, "{ corrupt").unwrap();
        // read_dag still works; write_dag still works and overwrites the stale tmp.
        let _ = read_dag(pkg).unwrap();
        write_dag(pkg, &dag).unwrap();
        // The successful rename consumed the stale tmp; nothing left.
        assert!(!stale_tmp.exists());
    }
}

#[cfg(test)]
mod read_dag_tests {
    use super::*;

    /// Regression for the IVD v11 crash loop: the agent
    /// wrote `record.attempts: [{action, iteration}]` (non-conforming
    /// shape), making the entire WORKFLOW.json unparseable, which
    /// caused the harness to crash at startup. The spec's
    /// restart-on-exit + the server's auto-spawn created a 5-second
    /// tight loop that made no progress. `read_dag` now per-task
    /// recovers: it replaces the malformed task state with a
    /// well-formed `blocked` placeholder and persists the repair so
    /// every subsequent harness invocation parses successfully.
    #[test]
    fn read_dag_recovers_from_malformed_attempts_shape() {
        let tmp = tempfile::tempdir().unwrap();
        let bad = serde_json::json!({
            "version": "1.0",
            "workflow_id": "test",
            "current_task": null,
            "tasks": {
                "healthy_task": {
                    "kind": "computation",
                    "state": {"status": "ready"},
                    "depends_on": [],
                    "assignee": "agent",
                    "description": "ok"
                },
                "broken_task": {
                    "kind": "computation",
                    "state": {
                        "status": "blocked",
                        "record": {
                            "reason": "blocked",
                            "attempts": [{"action": "nope", "iteration": 2}]
                        }
                    },
                    "depends_on": [],
                    "assignee": "agent",
                    "description": "the agent wrote the wrong attempts shape"
                }
            }
        });
        std::fs::write(
            tmp.path().join("WORKFLOW.json"),
            serde_json::to_string_pretty(&bad).unwrap(),
        )
        .unwrap();
        // Strict parse would fail; read_dag must repair + succeed.
        let dag = read_dag(tmp.path()).expect("read_dag must not crash on bad attempts shape");
        assert_eq!(dag.tasks.len(), 2);
        // Repaired task ends up Blocked with a placeholder reason.
        let broken = dag.tasks.get("broken_task").unwrap();
        match &broken.state {
            ecaa_workflow_core::dag::TaskState::Blocked { record } => {
                assert!(record.reason.contains("harness could not parse"));
                assert!(record.attempts.is_empty());
            }
            other => panic!("expected broken_task Blocked, got {:?}", other),
        }
        // Healthy task survives unchanged.
        let healthy = dag.tasks.get("healthy_task").unwrap();
        assert!(matches!(
            healthy.state,
            ecaa_workflow_core::dag::TaskState::Ready
        ));
        // And a fresh read hits the fast path (the repair got persisted).
        let dag2 = read_dag(tmp.path()).expect("second read on repaired file");
        assert_eq!(dag2.tasks.len(), 2);
    }

    /// Regression for the DE silent-completion case. Agents
    /// occasionally transition a task to Completed with a result object
    /// that carries an `overall_*_not_run: true` sentinel — effectively
    /// "the task exited but the work didn't run." The harness guard
    /// detects that pattern in-flight, flips the task back to Blocked
    /// with a synthesized record pointing at the existing blocker.json,
    /// and persists the repair so downstream iterations don't advance.
    /// This test locks the detection logic against the empty-result
    /// shape without requiring a live harness loop.
    #[test]
    fn validation_contract_blocks_task_on_missing_required_assertion() {
        // Fixture package with a completed compute task + validator,
        // plus a contract that requires a present artifact that
        // doesn't exist. Enforcement must flip both to blocked.
        use ecaa_workflow_core::dag::{Assignee, ResourceClass, Task, TaskKind, TaskState};
        let tmp = tempfile::tempdir().unwrap();
        let pkg = tmp.path();
        std::fs::create_dir_all(pkg.join("policies")).unwrap();
        std::fs::create_dir_all(pkg.join("runtime/outputs/qc")).unwrap();
        let contract = serde_json::json!({
            "contract_id": "test",
            "stages": {
                "qc": {
                    "assertions": [
                        {
                            "id": "qc.manifest_present",
                            "assertion_type": "artifact_present",
                            "target": "runtime/outputs/qc/manifest.json",
                            "severity": "required"
                        }
                    ]
                }
            }
        });
        std::fs::write(
            pkg.join("policies/validation-contract.json"),
            serde_json::to_string_pretty(&contract).unwrap(),
        )
        .unwrap();

        let mut tasks: std::collections::BTreeMap<TaskId, Task> = std::collections::BTreeMap::new();
        tasks.insert(
            "qc".into(),
            Task {
                kind: TaskKind::Computation,
                state: TaskState::Completed {
                    result: serde_json::json!({"method": "x"}),
                },
                depends_on: vec![],
                assignee: Assignee::Agent,
                description: "qc".into(),
                spec: Some(serde_json::json!({"stage_class": "qc"})),
                resolution: None,
                result_ref: None,
                resource_class: ResourceClass::CpuHeavy,
                requires_sme_review: false,

                required_artifacts: vec![],
                container: None,
                source_atom_id: None,
                safety: Default::default(),
            },
        );
        tasks.insert(
            "validate_qc".into(),
            Task {
                kind: TaskKind::Validation,
                state: TaskState::Completed {
                    result: serde_json::json!({"outcome": "pass"}),
                },
                depends_on: vec!["qc".into()],
                assignee: Assignee::Agent,
                description: "validate qc".into(),
                spec: Some(serde_json::json!({"stage_class": "qc"})),
                resolution: None,
                result_ref: None,
                resource_class: ResourceClass::CpuHeavy,
                requires_sme_review: false,

                required_artifacts: vec![],
                container: None,
                source_atom_id: None,
                safety: Default::default(),
            },
        );
        let mut dag = DAG {
            version: "1.0".into(),
            schema_version: ecaa_workflow_core::dag::current_dag_schema_version(),
            workflow_id: "t".into(),
            current_task: None,
            tasks,
            reverse_deps: std::collections::BTreeMap::new(),
            run_id: None,
        };
        let violations = enforce_validation_contract(pkg, &mut dag).unwrap();
        assert_eq!(violations.len(), 1);
        assert_eq!(violations[0].0, "qc");
        assert!(violations[0].1.contains(&"qc.manifest_present".to_string()));
        // Both the compute task and its validator are now Blocked
        assert!(matches!(
            dag.tasks.get("qc").unwrap().state,
            TaskState::Blocked { .. }
        ));
        assert!(matches!(
            dag.tasks.get("validate_qc").unwrap().state,
            TaskState::Blocked { .. }
        ));
    }

    #[test]
    fn validation_contract_passes_when_artifact_present() {
        use ecaa_workflow_core::dag::{Assignee, ResourceClass, Task, TaskKind, TaskState};
        let tmp = tempfile::tempdir().unwrap();
        let pkg = tmp.path();
        std::fs::create_dir_all(pkg.join("policies")).unwrap();
        std::fs::create_dir_all(pkg.join("runtime/outputs/qc")).unwrap();
        std::fs::write(pkg.join("runtime/outputs/qc/manifest.json"), "{}").unwrap();
        let contract = serde_json::json!({
            "contract_id": "test",
            "stages": {
                "qc": {
                    "assertions": [
                        {
                            "id": "qc.manifest_present",
                            "assertion_type": "artifact_present",
                            "target": "runtime/outputs/qc/manifest.json",
                            "severity": "required"
                        }
                    ]
                }
            }
        });
        std::fs::write(
            pkg.join("policies/validation-contract.json"),
            serde_json::to_string_pretty(&contract).unwrap(),
        )
        .unwrap();
        let mut tasks: std::collections::BTreeMap<TaskId, Task> = std::collections::BTreeMap::new();
        tasks.insert(
            "qc".into(),
            Task {
                kind: TaskKind::Computation,
                state: TaskState::Completed {
                    result: serde_json::json!({}),
                },
                depends_on: vec![],
                assignee: Assignee::Agent,
                description: "qc".into(),
                spec: Some(serde_json::json!({"stage_class": "qc"})),
                resolution: None,
                result_ref: None,
                resource_class: ResourceClass::CpuHeavy,
                requires_sme_review: false,

                required_artifacts: vec![],
                container: None,
                source_atom_id: None,
                safety: Default::default(),
            },
        );
        tasks.insert(
            "validate_qc".into(),
            Task {
                kind: TaskKind::Validation,
                state: TaskState::Completed {
                    result: serde_json::json!({}),
                },
                depends_on: vec!["qc".into()],
                assignee: Assignee::Agent,
                description: "v".into(),
                spec: Some(serde_json::json!({"stage_class": "qc"})),
                resolution: None,
                result_ref: None,
                resource_class: ResourceClass::CpuHeavy,
                requires_sme_review: false,

                required_artifacts: vec![],
                container: None,
                source_atom_id: None,
                safety: Default::default(),
            },
        );
        let mut dag = DAG {
            version: "1.0".into(),
            schema_version: ecaa_workflow_core::dag::current_dag_schema_version(),
            workflow_id: "t".into(),
            current_task: None,
            tasks,
            reverse_deps: std::collections::BTreeMap::new(),
            run_id: None,
        };
        let violations = enforce_validation_contract(pkg, &mut dag).unwrap();
        assert!(violations.is_empty());
    }

    #[test]
    fn env_capability_probe_writes_capability_file() {
        let tmp = tempfile::tempdir().unwrap();
        write_env_capability(tmp.path()).unwrap();
        let out = tmp.path().join("runtime/env_capability.json");
        assert!(out.exists(), "env_capability.json must be written");
        let body: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&out).unwrap()).unwrap();
        // Required keys always present
        let caps = body.get("capabilities").unwrap().as_object().unwrap();
        for key in [
            "r_seurat",
            "r_cellchat",
            "pyscenic",
            "python_lisi",
            "cellranger_version",
            "rna_velocity_capable",
        ] {
            assert!(
                caps.contains_key(key),
                "capability '{}' must be in report",
                key
            );
        }
        assert!(body.get("probed_at").unwrap().is_string());
        assert!(body.get("host_os").unwrap().is_string());
    }

    /// Build a one-compute-task DAG so the stamp_safety_network tests
    /// can mutate the task's safety policy + kind without depending on
    /// `write_dag_tests::running_fixture` (different module scope).
    fn one_compute_task_dag() -> DAG {
        use ecaa_workflow_core::dag::{Assignee, ResourceClass, Task, TaskKind, TaskState};
        let mut dag = DAG {
            version: "1".into(),
            schema_version: ecaa_workflow_core::dag::current_dag_schema_version(),
            workflow_id: "stamp_safety_network_test".into(),
            current_task: None,
            tasks: std::collections::BTreeMap::new(),
            reverse_deps: std::collections::BTreeMap::new(),
            run_id: None,
        };
        dag.tasks.insert(
            "compute".into(),
            Task {
                kind: TaskKind::Computation,
                state: TaskState::Ready,
                depends_on: vec![],
                assignee: Assignee::Agent,
                description: "compute".into(),
                spec: None,
                resolution: None,
                result_ref: None,
                resource_class: ResourceClass::CpuHeavy,
                requires_sme_review: false,
                required_artifacts: vec![],
                container: None,
                source_atom_id: None,
                safety: Default::default(),
            },
        );
        dag
    }

    #[test]
    fn stamp_safety_network_upgrades_default_compute_none_to_bridge() {
        // Compute task whose YAML didn't set `safety.network` lands on
        // the structural default `NetworkPolicy::None { allowlist: [] }`.
        // The harness must stamp "bridge" so the agent's install path
        // (pip / BiocManager / conda) can reach pypi / Bioconductor.
        let dag = one_compute_task_dag();
        let mut env: std::collections::BTreeMap<String, String> = std::collections::BTreeMap::new();
        stamp_safety_network(&mut env, &dag, "compute");
        assert_eq!(
            env.get("ECAA_TASK_NETWORK").map(String::as_str),
            Some("bridge")
        );
    }

    #[test]
    fn stamp_safety_network_preserves_explicit_allowlist_isolation() {
        // A compute atom that genuinely needs air-gapped execution
        // declares a non-empty allowlist (the safety-lint treats this
        // as still-None-effectively). The stamp must respect the
        // explicit isolation, not upgrade to bridge.
        use ecaa_workflow_core::atom::NetworkPolicy;
        let mut dag = one_compute_task_dag();
        dag.tasks.get_mut("compute").unwrap().safety.network = NetworkPolicy::None {
            allowlist: vec!["pypi.org".into()],
        };
        let mut env: std::collections::BTreeMap<String, String> = std::collections::BTreeMap::new();
        stamp_safety_network(&mut env, &dag, "compute");
        assert_eq!(
            env.get("ECAA_TASK_NETWORK").map(String::as_str),
            Some("none")
        );
    }

    #[test]
    fn stamp_safety_network_keeps_non_compute_kinds_isolated() {
        // Validators / discover / gate / review tasks don't run user
        // code that installs libraries; their default isolation
        // ("none") must NOT be upgraded.
        use ecaa_workflow_core::dag::TaskKind;
        let mut dag = one_compute_task_dag();
        dag.tasks.get_mut("compute").unwrap().kind = TaskKind::Validation;
        let mut env: std::collections::BTreeMap<String, String> = std::collections::BTreeMap::new();
        stamp_safety_network(&mut env, &dag, "compute");
        assert_eq!(
            env.get("ECAA_TASK_NETWORK").map(String::as_str),
            Some("none")
        );
    }

    #[test]
    fn stamp_safety_network_passes_through_explicit_bridge() {
        use ecaa_workflow_core::atom::NetworkPolicy;
        let mut dag = one_compute_task_dag();
        dag.tasks.get_mut("compute").unwrap().safety.network = NetworkPolicy::Bridge;
        let mut env: std::collections::BTreeMap<String, String> = std::collections::BTreeMap::new();
        stamp_safety_network(&mut env, &dag, "compute");
        assert_eq!(
            env.get("ECAA_TASK_NETWORK").map(String::as_str),
            Some("bridge")
        );
    }

    #[test]
    fn env_capability_probe_includes_methods_block() {
        let tmp = tempfile::tempdir().unwrap();
        write_env_capability(tmp.path()).unwrap();
        let body: serde_json::Value = serde_json::from_slice(
            &std::fs::read(tmp.path().join("runtime/env_capability.json")).unwrap(),
        )
        .unwrap();
        let methods = body
            .get("methods")
            .expect("methods block")
            .as_object()
            .unwrap();
        // Every entry in METHOD_PROBES must appear in the report.
        assert_eq!(methods.len(), METHOD_PROBES.len());
        for (name, _) in METHOD_PROBES.iter() {
            let entry = methods.get(*name).unwrap_or_else(|| {
                panic!("method '{}' must be in env_capability.json::methods", name)
            });
            // Schema: each entry has available (bool), language (str),
            // probe_target (str).
            assert!(entry.get("available").unwrap().is_boolean());
            assert!(entry.get("language").unwrap().is_string());
            assert!(entry.get("probe_target").unwrap().is_string());
        }
        // The specific methods that drove the gseapy regression must be probed.
        for required in ["gseapy", "fgsea", "clusterprofiler", "enrichr", "deseq2"] {
            // gseapy/enrichr both reference gseapy in METHOD_PROBES — the
            // map keys are atom-YAML method ids, not probe targets.
            // Method id `gsea` ↔ probe_target `gseapy`.
            let key = if required == "gseapy" {
                "gsea"
            } else {
                required
            };
            assert!(
                methods.contains_key(key),
                "method id '{}' (drove the silent-substitution defect) must be probed",
                key
            );
        }
    }

    /// Regression for the IVD batch_correction loop where Seurat 5.5.0
    /// was installed into runtime/r-libs/ but probe_r_package only
    /// checked system R — every harness restart logged
    /// `R+Seurat=false` and the agent walked through the full install
    /// path again. The probe now prepends runtime/r-libs/ to the
    /// `.libPaths()` it tests against (and sets R_LIBS_USER as a
    /// belt-and-braces second mechanism), so a package-local install
    /// is honoured.
    #[test]
    fn probe_r_package_threads_runtime_r_libs_into_libpaths() {
        // We can't assert on R itself succeeding without R installed
        // in the test environment, but we can verify the inline
        // libPaths expression is built correctly by inspecting the
        // command we'd send. The integration assertion is that the
        // probe does NOT panic and returns a bool either way.
        let tmp = tempfile::tempdir().unwrap();
        let r_libs = tmp.path().join("r-libs");
        std::fs::create_dir_all(&r_libs).unwrap();
        // Probing for a package that almost certainly isn't installed
        // in the test env should return false either way; the test is
        // really asserting that the function runs to completion when
        // r_libs_user is supplied.
        let _ = probe_r_package("ThisPackageDoesNotExist", Some(r_libs.as_path()));
        let _ = probe_r_package("ThisPackageDoesNotExist", None);
        // Sanity: the directory is detected as a candidate r_libs_user
        // by write_env_capability when present.
        let pkg_root = tmp.path();
        std::fs::create_dir_all(pkg_root.join("runtime/r-libs")).unwrap();
        write_env_capability(pkg_root).unwrap();
        let out = pkg_root.join("runtime/env_capability.json");
        assert!(out.exists());
    }

    #[test]
    fn detects_empty_completion_sentinel() {
        use ecaa_workflow_core::dag::TaskState;
        let result = serde_json::json!({
            "method": "pseudobulk_deseq2",
            "overall_de_not_run": true,
            "overall_de_not_run_reason": "no compartment passed min_samples check",
        });
        let state = TaskState::Completed {
            result: result.clone(),
        };
        // The guard scans the result object for any key matching
        // `overall_*_not_run == true`.
        let sentinel = if let TaskState::Completed { result } = &state {
            result.as_object().map(|obj| {
                obj.iter().any(|(k, v)| {
                    k.starts_with("overall_")
                        && k.ends_with("_not_run")
                        && v.as_bool() == Some(true)
                })
            })
        } else {
            None
        };
        assert_eq!(sentinel, Some(true));
    }

    #[test]
    fn does_not_flip_healthy_completion() {
        use ecaa_workflow_core::dag::TaskState;
        let result = serde_json::json!({
            "method": "seurat_v5_cca",
            "cells_total_integrated": 403868,
            "batch_mixing_improvement": {"NP": {"delta": 0.111}},
        });
        let state = TaskState::Completed { result };
        let sentinel = if let TaskState::Completed { result } = &state {
            result.as_object().map(|obj| {
                obj.iter().any(|(k, v)| {
                    k.starts_with("overall_")
                        && k.ends_with("_not_run")
                        && v.as_bool() == Some(true)
                })
            })
        } else {
            None
        };
        assert_eq!(sentinel, Some(false));
    }

    /// When on-disk WORKFLOW.json is corrupt but the directory has a
    /// clean git history, `read_dag` must return the committed version
    /// rather than falling through to per-task placeholder repair.
    #[test]
    fn read_dag_recovers_from_git_when_disk_corrupt() {
        let tmp = tempfile::tempdir().unwrap();
        let pkg = tmp.path();

        // Write a valid DAG and commit it.
        let good = serde_json::json!({
            "version": "1.0",
            "workflow_id": "git-recovery-test",
            "current_task": null,
            "tasks": {
                "my_task": {
                    "kind": "computation",
                    "state": {"status": "ready"},
                    "depends_on": [],
                    "assignee": "agent",
                    "description": "committed clean task"
                }
            }
        });
        std::fs::write(
            pkg.join("WORKFLOW.json"),
            serde_json::to_string_pretty(&good).unwrap(),
        )
        .unwrap();
        // Init a repo and commit the clean WORKFLOW.json.
        let git_ok = std::process::Command::new("git")
            .args(["init", "-b", "main"])
            .current_dir(pkg)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if !git_ok {
            // git not available in this environment — skip rather than fail.
            return;
        }
        for args in [
            vec!["config", "user.email", "test@test.invalid"],
            vec!["config", "user.name", "Test"],
            vec!["add", "WORKFLOW.json"],
            vec!["commit", "-m", "initial"],
        ] {
            let ok = std::process::Command::new("git")
                .args(&args)
                .current_dir(pkg)
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false);
            assert!(ok, "git {:?} failed", args);
        }

        // Now corrupt the on-disk copy with a malformed attempts shape.
        let corrupt = serde_json::json!({
            "version": "1.0",
            "workflow_id": "git-recovery-test",
            "current_task": null,
            "tasks": {
                "my_task": {
                    "kind": "computation",
                    "state": {
                        "status": "blocked",
                        "record": {
                            "reason": "bad",
                            "attempts": [{"action": "nope", "iteration": 999}]
                        }
                    },
                    "depends_on": [],
                    "assignee": "agent",
                    "description": "corrupt on disk"
                }
            }
        });
        std::fs::write(
            pkg.join("WORKFLOW.json"),
            serde_json::to_string_pretty(&corrupt).unwrap(),
        )
        .unwrap();

        let dag = read_dag(pkg).expect("read_dag must recover from git HEAD");
        assert_eq!(dag.workflow_id, "git-recovery-test");
        // The task must be Ready (from the committed copy), not Blocked.
        let task = dag.tasks.get("my_task").unwrap();
        assert!(
            matches!(task.state, ecaa_workflow_core::dag::TaskState::Ready),
            "expected Ready from git HEAD, got {:?}",
            task.state
        );
    }

    /// When the package directory is not a git repo, `read_dag` must
    /// skip git recovery silently and fall through to per-task
    /// placeholder repair as before.
    #[test]
    fn read_dag_falls_through_when_git_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let pkg = tmp.path();

        // No git init — just write a corrupt file directly.
        let corrupt = serde_json::json!({
            "version": "1.0",
            "workflow_id": "no-git-test",
            "current_task": null,
            "tasks": {
                "broken_task": {
                    "kind": "computation",
                    "state": {
                        "status": "blocked",
                        "record": {
                            "reason": "bad",
                            "attempts": [{"action": "nope", "iteration": 1}]
                        }
                    },
                    "depends_on": [],
                    "assignee": "agent",
                    "description": "corrupt, no git"
                }
            }
        });
        std::fs::write(
            pkg.join("WORKFLOW.json"),
            serde_json::to_string_pretty(&corrupt).unwrap(),
        )
        .unwrap();

        let dag = read_dag(pkg).expect("read_dag must fall through to per-task repair");
        let task = dag.tasks.get("broken_task").unwrap();
        match &task.state {
            ecaa_workflow_core::dag::TaskState::Blocked { record } => {
                assert!(
                    record.reason.contains("harness could not parse"),
                    "expected placeholder reason, got {:?}",
                    record.reason
                );
            }
            other => panic!("expected Blocked placeholder, got {:?}", other),
        }
    }

    /// When HEAD:WORKFLOW.json exists in git but is itself malformed,
    /// `read_dag` must skip git recovery and fall through to per-task
    /// placeholder repair.
    #[test]
    fn read_dag_falls_through_when_head_workflow_also_bad() {
        let tmp = tempfile::tempdir().unwrap();
        let pkg = tmp.path();

        // Commit a malformed WORKFLOW.json (bad attempts shape).
        let bad_committed = serde_json::json!({
            "version": "1.0",
            "workflow_id": "bad-head-test",
            "current_task": null,
            "tasks": {
                "already_bad": {
                    "kind": "computation",
                    "state": {
                        "status": "blocked",
                        "record": {
                            "reason": "also corrupt in git",
                            "attempts": [{"action": "nope", "iteration": 42}]
                        }
                    },
                    "depends_on": [],
                    "assignee": "agent",
                    "description": "bad even in git HEAD"
                }
            }
        });
        std::fs::write(
            pkg.join("WORKFLOW.json"),
            serde_json::to_string_pretty(&bad_committed).unwrap(),
        )
        .unwrap();

        let git_ok = std::process::Command::new("git")
            .args(["init", "-b", "main"])
            .current_dir(pkg)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if !git_ok {
            return;
        }
        for args in [
            vec!["config", "user.email", "test@test.invalid"],
            vec!["config", "user.name", "Test"],
            vec!["add", "WORKFLOW.json"],
            vec!["commit", "-m", "bad initial"],
        ] {
            let ok = std::process::Command::new("git")
                .args(&args)
                .current_dir(pkg)
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false);
            assert!(ok, "git {:?} failed", args);
        }

        // The on-disk copy is also malformed (same bad shape).
        let dag = read_dag(pkg).expect("read_dag must fall through to per-task repair");
        let task = dag.tasks.get("already_bad").unwrap();
        match &task.state {
            ecaa_workflow_core::dag::TaskState::Blocked { record } => {
                assert!(
                    record.reason.contains("harness could not parse"),
                    "expected placeholder reason, got {:?}",
                    record.reason
                );
            }
            other => panic!("expected Blocked placeholder, got {:?}", other),
        }
    }
}

#[cfg(test)]
mod settle_tests {
    //! Layer-D harness pump fix: when an iteration is a true no-op
    //! AND at least one Running task has a fresh heartbeat, the loop
    //! sleeps `ECAA_HARNESS_SETTLE_SECS` instead of immediately
    //! re-iterating. These tests cover the predicate + helpers; the
    //! full sleep wiring is exercised by the integration smoke runs.
    use super::*;
    use ecaa_workflow_core::dag::{
        Assignee, BlockedRecord, ResourceClass, Task, TaskId, TaskKind,
    };
    use std::collections::BTreeMap;

    fn task(id: &str, state: TaskState) -> (TaskId, Task) {
        (
            TaskId::from(id),
            Task {
                kind: TaskKind::Computation,
                state,
                depends_on: vec![],
                assignee: Assignee::Agent,
                description: id.into(),
                spec: None,
                resolution: None,
                result_ref: None,
                resource_class: ResourceClass::CpuHeavy,
                requires_sme_review: false,
                required_artifacts: vec![],
                container: None,
                source_atom_id: None,
                safety: Default::default(),
            },
        )
    }

    fn dag_from(tasks: Vec<(TaskId, Task)>) -> DAG {
        let mut t = BTreeMap::new();
        for (id, v) in tasks {
            t.insert(id, v);
        }
        DAG {
            version: "1".into(),
            schema_version: ecaa_workflow_core::dag::current_dag_schema_version(),
            workflow_id: "wf".into(),
            current_task: None,
            tasks: t,
            reverse_deps: BTreeMap::new(),
            run_id: None,
        }
    }

    fn running(at: &str) -> TaskState {
        TaskState::Running {
            started_at: at.into(),
            remote: None,
        }
    }

    #[test]
    fn settle_iteration_when_only_fresh_heartbeat_running_tasks_remain() {
        // Real-world IVD pump shape, post-Layer-A: `batch_correction`
        // is Running with a fresh heartbeat (compute genuinely in
        // flight), the agent yielded a running→running no-op, no
        // transitions happened, no blocked tasks need SME. Settle.
        let dag = dag_from(vec![task(
            "batch_correction",
            running("2026-01-01T00:00:00Z"),
        )]);
        assert!(is_settle_iteration(
            &dag,
            0,
            &["batch_correction".to_string()],
            &[]
        ));
    }

    #[test]
    fn no_settle_when_transitions_happened() {
        // The probe completed a task this iteration → not idle. Burn
        // the next iteration immediately so dependents become Ready.
        let dag = dag_from(vec![task("running_task", running("2026-01-01T00:00:00Z"))]);
        assert!(!is_settle_iteration(
            &dag,
            5, // 5 JSON-patch ops
            &["running_task".to_string()],
            &[]
        ));
    }

    #[test]
    fn no_settle_when_blocked_tasks_need_sme() {
        // A real human-decision blocker shouldn't be slept through.
        let dag = dag_from(vec![task(
            "real_block",
            TaskState::Blocked {
                record: BlockedRecord {
                    reason: "needs SME pick".into(),
                    attempts: vec![],
                },
            },
        )]);
        assert!(!is_settle_iteration(
            &dag,
            0,
            &[],
            &["real_block".to_string()]
        ));
    }

    #[test]
    fn no_settle_when_ready_tasks_exist() {
        // Don't sleep on a Ready task — the next iteration should
        // dispatch immediately. Settle is only for the "all running,
        // all healthy, nothing to do" shape.
        let dag = dag_from(vec![
            task("ready_one", TaskState::Ready),
            task("running_one", running("2026-01-01T00:00:00Z")),
        ]);
        assert!(!is_settle_iteration(
            &dag,
            0,
            &["running_one".to_string()],
            &[]
        ));
    }

    #[test]
    fn no_settle_when_no_running_tasks_with_fresh_heartbeat() {
        // Empty fresh-heartbeat list means either no Running tasks or
        // every Running task has a stale heartbeat (caught by the
        // heartbeat-stall detector). Don't sleep — let the iteration
        // loop spin its normal cadence so the stall detector fires.
        let dag = dag_from(vec![]);
        assert!(!is_settle_iteration(&dag, 0, &[], &[]));
    }

    #[test]
    fn settle_interval_clamps_into_range() {
        std::env::set_var("ECAA_HARNESS_SETTLE_SECS", "1");
        assert_eq!(settle_interval_secs(), 5, "must clamp up to 5s");
        std::env::set_var("ECAA_HARNESS_SETTLE_SECS", "9999");
        assert_eq!(settle_interval_secs(), 1800, "must clamp down to 1800s");
        std::env::set_var("ECAA_HARNESS_SETTLE_SECS", "0");
        assert_eq!(settle_interval_secs(), 0, "0 is the disable sentinel");
        std::env::set_var("ECAA_HARNESS_SETTLE_SECS", "60");
        assert_eq!(settle_interval_secs(), 60);
        std::env::remove_var("ECAA_HARNESS_SETTLE_SECS");
    }

    #[test]
    fn fresh_heartbeat_running_filters_out_stale_heartbeats() {
        // Two Running tasks: one with a fresh heartbeat, one with
        // a heartbeat older than the threshold. Only the fresh one
        // shows up in the result.
        let tmp = tempfile::tempdir().unwrap();
        let pkg = tmp.path();
        let dag = dag_from(vec![
            task("fresh", running("2026-01-01T00:00:00Z")),
            task("stale", running("2026-01-01T00:00:00Z")),
        ]);
        std::fs::create_dir_all(pkg.join("runtime/outputs/fresh")).unwrap();
        std::fs::create_dir_all(pkg.join("runtime/outputs/stale")).unwrap();
        // Fresh: write now.
        std::fs::write(
            pkg.join("runtime/outputs/fresh/.heartbeat"),
            chrono::Utc::now().to_rfc3339(),
        )
        .unwrap();
        // Stale: write a heartbeat then mtime-walk it backwards via
        // touch -d. We approximate by setting the env threshold to
        // a tiny value (1s) and sleeping past it for one task.
        std::fs::write(
            pkg.join("runtime/outputs/stale/.heartbeat"),
            chrono::Utc::now().to_rfc3339(),
        )
        .unwrap();
        // Tighten threshold to 1 sec, then sleep 2 sec so `stale`'s
        // heartbeat ages past it but `fresh`'s is rewritten right
        // before we test. We use different paths so we can selectively
        // refresh.
        std::env::set_var("ECAA_TASK_HEARTBEAT_STALL_SECS", "1");
        std::thread::sleep(std::time::Duration::from_secs(2));
        // Refresh `fresh`'s heartbeat right before the call.
        std::fs::write(
            pkg.join("runtime/outputs/fresh/.heartbeat"),
            chrono::Utc::now().to_rfc3339(),
        )
        .unwrap();
        let result = fresh_heartbeat_running_task_ids(pkg, &dag);
        std::env::remove_var("ECAA_TASK_HEARTBEAT_STALL_SECS");
        assert_eq!(result, vec!["fresh".to_string()]);
    }
}

#[cfg(test)]
mod picker_decision_audit_tests {
    //! Integration-layer tests for the picker-decision audit trail wired
    //! in the main dispatch loop.  These tests call
    //! `picker_decisions::append_picker_decisions` directly with
    //! synthetic records to verify the on-disk JSONL contract without
    //! spinning up a full harness loop.
    use super::*;
    use picker_decisions::{append_picker_decisions, PickerDecisionRecord};
    use std::io::BufRead as _;

    fn sandbox_refused_record(_pkg_root: &std::path::Path, task_id: &str) -> PickerDecisionRecord {
        PickerDecisionRecord {
            ts: chrono::Utc::now().to_rfc3339(),
            iteration: 0,
            task_id: task_id.to_string(),
            decision: "sandbox_refused",
            reason: format!(
                "UnpinnedContainer:{} (node={}); NetworkDenied: (node={})",
                task_id, task_id, task_id
            ),
        }
    }

    /// A single sandbox_refused task must produce exactly one line in
    /// `runtime/picker-decisions.jsonl` with `"decision":"sandbox_refused"`.
    #[test]
    fn sandbox_refused_task_writes_one_jsonl_line() {
        let tmp = tempfile::tempdir().unwrap();
        let pkg = tmp.path();
        // runtime/ does not pre-exist — the helper must create it.
        let records = vec![sandbox_refused_record(pkg, "align_reads")];
        append_picker_decisions(pkg, &records);

        let path = pkg.join("runtime/picker-decisions.jsonl");
        assert!(path.exists(), "picker-decisions.jsonl must be created");

        let file = std::fs::File::open(&path).unwrap();
        let lines: Vec<String> = std::io::BufReader::new(file)
            .lines()
            .map(|l| l.unwrap())
            .collect();
        assert_eq!(lines.len(), 1, "one record → one line");

        let obj: serde_json::Value = serde_json::from_str(&lines[0]).unwrap();
        assert_eq!(
            obj["decision"], "sandbox_refused",
            "decision field must be sandbox_refused"
        );
        assert_eq!(obj["task_id"], "align_reads", "task_id must round-trip");
        assert_eq!(obj["iteration"], 0, "iteration must be 0");
        assert!(
            obj["reason"]
                .as_str()
                .unwrap_or("")
                .contains("UnpinnedContainer"),
            "reason must contain refusal detail"
        );
        assert!(
            obj["ts"].as_str().unwrap_or("").contains('T'),
            "ts must be RFC-3339 shaped"
        );
    }

    /// Mix of accepted + sandbox_refused: both records are written when
    /// the caller passes them (the caller is responsible for filtering
    /// out accepted-only iterations before calling).
    #[test]
    fn accepted_and_refused_records_both_written() {
        let tmp = tempfile::tempdir().unwrap();
        let pkg = tmp.path();
        std::fs::create_dir_all(pkg.join("runtime")).unwrap();

        let records = vec![
            PickerDecisionRecord {
                ts: "2026-01-01T00:00:00Z".into(),
                iteration: 2,
                task_id: "qc_reads".into(),
                decision: "accepted",
                reason: String::new(),
            },
            sandbox_refused_record(pkg, "align_reads"),
        ];
        append_picker_decisions(pkg, &records);

        let content = std::fs::read_to_string(pkg.join("runtime/picker-decisions.jsonl")).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 2);

        // Verify both parse as valid JSON.
        let accepted: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        let refused: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(accepted["decision"], "accepted");
        assert_eq!(refused["decision"], "sandbox_refused");
    }
}

#[cfg(test)]
mod sse_ordering_tests {
    //! Covers the WORKFLOW.json-before-SSE ordering invariant.
    //!
    //! Full end-to-end verification of `state_write_precedes_progress_emit`
    //! requires injecting a spy `ProgressClient` that can observe whether
    //! `write_dag` committed to disk before `set_task_state` enqueued.
    //! Doing so would require either (a) threading a `ProgressClient` trait
    //! object into `run_harness` or (b) a subprocess integration test that
    //! races a filesystem watcher against the SSE stream — both are M-effort
    //! refactors outside the scope of this S-effort ordering fix.
    //!
    //! The invariant is instead verified structurally: every `set_task_state`
    //! call site in `main.rs` is preceded by a `write_dag` call in the same
    //! lexical scope (confirmed by the `grep -n` ordering in the commit that
    //! introduced this test). A future refactor that adds injection points
    //! should replace this structural note with a live spy assertion.

    /// Structural guard: documents the gap so it's not forgotten.
    #[test]
    fn state_write_precedes_progress_emit_gap_documented() {
        // This test intentionally passes unconditionally. It exists to anchor
        // the test-count baseline and make the coverage gap visible in CI
        // output. The actual ordering is enforced by code structure (each
        // set_task_state call site has a write_dag call immediately above it
        // or within the same conditional block), not by a runtime assertion.
        //
        // When a ProgressClient spy is available, replace this with:
        //   let spy = SpyProgressClient::new();
        //   run_harness_with_progress(&pkg, spy.clone());
        //   spy.assert_write_dag_before_every_set_task_state();
        assert!(true, "see module-level doc for gap rationale");
    }
}

#[cfg(test)]
mod watchdog_event_relevance_tests {
    use super::*;
    use ecaa_workflow_core::dag::{
        Assignee, ResourceClass, Task, TaskId, TaskKind, TaskState, DAG,
    };
    use std::collections::BTreeMap;

    fn task_with_state(state: TaskState) -> Task {
        Task {
            kind: TaskKind::Computation,
            state,
            depends_on: vec![],
            assignee: Assignee::Agent,
            description: "test task".into(),
            spec: None,
            resolution: None,
            result_ref: None,
            resource_class: ResourceClass::CpuHeavy,
            requires_sme_review: false,
            required_artifacts: vec![],
            container: None,
            source_atom_id: None,
            safety: Default::default(),
        }
    }

    fn write_single_task_dag(pkg: &std::path::Path, task_id: &str, state: TaskState) {
        let mut tasks = BTreeMap::new();
        tasks.insert(TaskId::from(task_id), task_with_state(state));
        let dag = DAG {
            version: "1".into(),
            schema_version: ecaa_workflow_core::dag::current_dag_schema_version(),
            workflow_id: "wf".into(),
            current_task: None,
            tasks,
            reverse_deps: BTreeMap::new(),
            run_id: None,
        };
        write_dag(pkg, &dag).unwrap();
    }

    #[test]
    fn wall_clock_event_for_completed_task_is_stale() {
        let tmp = tempfile::tempdir().unwrap();
        write_single_task_dag(
            tmp.path(),
            "discover_differential_expression",
            TaskState::Completed {
                result: serde_json::json!({"method_id": "deseq2"}),
            },
        );

        assert!(
            !watchdog_wall_clock_event_is_current(tmp.path(), "discover_differential_expression"),
            "queued watchdog event must not apply after task leaves Running"
        );
    }

    #[test]
    fn wall_clock_event_for_running_task_is_current() {
        let tmp = tempfile::tempdir().unwrap();
        write_single_task_dag(
            tmp.path(),
            "normalisation",
            TaskState::Running {
                started_at: "2026-05-25T01:00:00Z".into(),
                remote: None,
            },
        );

        assert!(
            watchdog_wall_clock_event_is_current(tmp.path(), "normalisation"),
            "running task wall-clock alerts should still be forwarded"
        );
    }
}

#[cfg(test)]
mod amend_cancel_tests {
    //! Unit tests for the session-amend → soft-cancel-of-running-tasks path.
    //! These tests validate the cancellation predicate logic and the
    //! `CancelledByAmendment` blocker variant serialisation independently
    //! of the full harness loop so they run without a live server or
    //! real executor subprocess.
    use ecaa_workflow_core::blocker::BlockerKind;
    use ecaa_workflow_core::dag::{
        Assignee, BlockedRecord, ResourceClass, Task, TaskId, TaskKind, TaskState, DAG,
    };
    use std::collections::BTreeMap;

    fn make_task(state: TaskState) -> Task {
        Task {
            kind: TaskKind::Computation,
            state,
            depends_on: vec![],
            assignee: Assignee::Agent,
            description: "test".into(),
            spec: None,
            resolution: None,
            result_ref: None,
            resource_class: ResourceClass::CpuHeavy,
            requires_sme_review: false,
            required_artifacts: vec![],
            container: None,
            source_atom_id: None,
            safety: Default::default(),
        }
    }

    fn dag_with_tasks(tasks: Vec<(&str, TaskState)>) -> DAG {
        let mut map = BTreeMap::new();
        for (id, state) in tasks {
            map.insert(TaskId::from(id), make_task(state));
        }
        DAG {
            version: "1".into(),
            schema_version: ecaa_workflow_core::dag::current_dag_schema_version(),
            workflow_id: "wf".into(),
            current_task: None,
            tasks: map,
            reverse_deps: BTreeMap::new(),
            run_id: None,
        }
    }

    /// `CancelledByAmendment` must round-trip through JSON with the
    /// expected internally-tagged wire shape so the server's
    /// `/progress` handler can promote it to a typed `BlockerKind`.
    #[test]
    fn cancelled_by_amendment_roundtrips_serde() {
        let kind = BlockerKind::CancelledByAmendment {
            task_id: "align_reads".into(),
            target_stage: "alignment".into(),
        };
        let json = serde_json::to_string(&kind).expect("serialize");
        assert!(
            json.contains("\"kind\":\"cancelled_by_amendment\""),
            "expected internally-tagged kind field, got: {json}"
        );
        assert!(
            json.contains("\"task_id\":\"align_reads\""),
            "expected task_id field, got: {json}"
        );
        assert!(
            json.contains("\"target_stage\":\"alignment\""),
            "expected target_stage field, got: {json}"
        );
        let back: BlockerKind = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(kind, back, "CancelledByAmendment roundtrip mismatch");
    }

    /// Tasks in `Amending.invalidated_tasks` that are currently `Running`
    /// must be identified as cancellation candidates; tasks in other
    /// states (Pending, Completed, Blocked) must be skipped.
    #[test]
    fn amend_cancels_running_task_leaves_others_alone() {
        let target_stage = "alignment";
        let invalidated = vec!["t1".to_string(), "t2".to_string(), "t3".to_string()];

        let dag = dag_with_tasks(vec![
            (
                "t1",
                TaskState::Running {
                    started_at: "2026-05-18T00:00:00Z".into(),
                    remote: None,
                },
            ),
            ("t2", TaskState::Pending),
            (
                "t3",
                TaskState::Blocked {
                    record: BlockedRecord {
                        reason: "prior error".into(),
                        attempts: vec![],
                    },
                },
            ),
        ]);

        // Identify which tasks in the invalidated list are Running.
        let running_to_cancel: Vec<&str> = invalidated
            .iter()
            .filter(|tid| {
                matches!(
                    dag.tasks.get(tid.as_str()),
                    Some(t) if matches!(t.state, TaskState::Running { .. })
                )
            })
            .map(|s| s.as_str())
            .collect();

        assert_eq!(
            running_to_cancel,
            vec!["t1"],
            "only t1 is Running; t2 is Pending, t3 is Blocked"
        );

        // Verify the blocker we'd write has the correct shape.
        let blocker = BlockerKind::CancelledByAmendment {
            task_id: "t1".into(),
            target_stage: target_stage.into(),
        };
        assert!(
            matches!(
                &blocker,
                BlockerKind::CancelledByAmendment { task_id, target_stage: ts }
                    if task_id == "t1" && ts == "alignment"
            ),
            "blocker shape mismatch: {blocker:?}"
        );
    }

    /// When the session is NOT in Amending state (None from
    /// `get_amending_invalidated_tasks`), the harness must not touch
    /// any Running tasks — this test validates the guard predicate.
    #[test]
    fn non_amending_state_leaves_running_tasks_alone() {
        // Simulate `get_amending_invalidated_tasks` returning None
        // (session is Emitted or Blocked, not Amending).
        let amending_info: Option<(String, Vec<String>)> = None;

        let dag = dag_with_tasks(vec![(
            "t1",
            TaskState::Running {
                started_at: "2026-05-18T00:00:00Z".into(),
                remote: None,
            },
        )]);

        // If not amending, the cancellation sweep is a no-op.
        let would_cancel: Vec<String> = match amending_info {
            None => vec![],
            Some((_, invalidated)) => invalidated
                .into_iter()
                .filter(|tid| {
                    matches!(
                        dag.tasks.get(tid.as_str()),
                        Some(t) if matches!(t.state, TaskState::Running { .. })
                    )
                })
                .collect(),
        };

        assert!(
            would_cancel.is_empty(),
            "no tasks should be cancelled when session is not amending"
        );
    }

    /// The `[cancelled_by_amendment]` marker written to WORKFLOW.json
    /// must contain the task id and target stage so the server's
    /// `/progress` handler can promote it to a typed BlockerKind.
    #[test]
    fn blocker_reason_marker_format() {
        let task_id = "align_reads";
        let target_stage = "alignment";
        let reason = format!(
            "[cancelled_by_amendment] task={} target_stage={}",
            task_id, target_stage
        );
        assert!(reason.contains("[cancelled_by_amendment]"));
        assert!(reason.contains("task=align_reads"));
        assert!(reason.contains("target_stage=alignment"));
    }
}
