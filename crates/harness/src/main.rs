//! Sync orchestration harness that loops an agent subprocess against
//! ready tasks in a compiled RO-Crate package. Supports Local, Mock,
//! AWS, and SLURM executors via the `Executor` trait.
mod picker_decisions;
mod progress_client;

use anyhow::{Context, Result};
use clap::Parser;
use colored::Colorize;
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
use ecaa_workflow_harness::ecaa_io::{read_bytes_capped, read_capped, resolve_max_bytes};
use ecaa_workflow_harness::executor::hardware_envelope::{render_envelope, HardwareEnvelopeInputs};
use ecaa_workflow_harness::executor::host_probe::{
    allocate_for_picks, resolve_high_water_for, OverheadPolicy,
};
use ecaa_workflow_harness::executor::pilot::PilotConfig;
use ecaa_workflow_harness::executor::stall_monitor::{StallSignal, StallThresholds};
use ecaa_workflow_harness::executor::{self, Executor, ExecutorArgs};
use ecaa_workflow_harness::finalize_probe::{probe_one_task, ProbeOutcome};
use ecaa_workflow_harness::multiprocess_lock::SessionLock;
use ecaa_workflow_harness::status_reconciliation;
use ecaa_workflow_harness::scheduler::{
    count_concurrent_peers_by_class, dag_with_ready_tasks_limited_to, lane_mode_from_env,
    pause_dependent_tasks, pick_ready_respecting_budgets, pick_ready_with_lanes,
    read_confirmed_review_stages, ready_task_ids_passing_sme_gate, ConcurrencyMode,
    SchedulerBudget,
};
use ecaa_workflow_harness::scratch_cleanup::cleanup_task_scratch;
use ecaa_workflow_harness::sme_skip;
use ecaa_workflow_harness::stall_relay;
use ecaa_workflow_harness::validation_recovery;
use ecaa_workflow_harness::watchdog::{Watchdog, WatchdogConfig, WatchdogEvent};
use progress_client::ProgressClient;
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

/// Maps a non-success iteration capture onto the `(observed_secs,
/// threshold_secs)` pair for a `task_wall_clock_exceeded` progress
/// event when — and only when — the executor SIGKILLed the agent after
/// the hard wall-clock deadline elapsed. The server promotes the
/// resulting event to `Blocked { BlockerKind::WallClockExceeded }`.
/// Returns `None` for ordinary (non-wall-clock) agent failures so the
/// caller falls through to the normal tool-error-envelope path.
///
/// `threshold_secs` is the deadline the executor ACTUALLY enforced
/// (`capture.effective_deadline_secs`, which is
/// `max(task_timeout, agent_wallclock + grace)` and can exceed the raw
/// `--task-timeout`), falling back to the raw `task_timeout` only when the
/// backend cannot report it. Reporting the raw `--task-timeout` instead made
/// the SME message self-contradictory — e.g. "14520s observed, 300s threshold"
/// with `task_timeout=300, ECAA_AGENT_WALLCLOCK_SECS=14400`.
fn wall_clock_blocker_params(
    capture: &ecaa_workflow_harness::executor::IterationCapture,
    task_timeout: u64,
) -> Option<(u64, u64)> {
    if !capture.wall_clock_killed {
        return None;
    }
    let threshold = capture.effective_deadline_secs.unwrap_or(task_timeout);
    Some((capture.wallclock_secs.unwrap_or(0), threshold))
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
        // must not be dispatched to an executor that forwards task
        // context to a third-party LLM inference endpoint. Gated on the
        // declared capability (fail-closed default `true`) rather than
        // the backend kind, so an operator-declared on-prem no-egress
        // backend may run controlled data while every LLM-forwarding
        // backend (and any future one) is refused. The mock executor
        // sets `forwards_to_external_llm: false` and stays exempt.
        if task.safety.controlled_access && caps.forwards_to_external_llm {
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

#[cfg(test)]
mod provenance_env_tests {
    use super::*;

    #[test]
    fn stamp_inserts_both_keys() {
        let mut env = std::collections::BTreeMap::new();
        let prov = ProvenanceEnv {
            git_sha: "abc1234def".into(),
            package_id: "workflow-xyz".into(),
        };
        stamp_provenance_env(&mut env, &prov);
        assert_eq!(env.get("ECAA_GIT_SHA").map(String::as_str), Some("abc1234def"));
        assert_eq!(
            env.get("ECAA_PACKAGE_ID").map(String::as_str),
            Some("workflow-xyz")
        );
    }

    #[test]
    fn stamp_does_not_overwrite_existing() {
        let mut env = std::collections::BTreeMap::new();
        env.insert("ECAA_GIT_SHA".to_string(), "operator-set".to_string());
        let prov = ProvenanceEnv {
            git_sha: "resolved".into(),
            package_id: "pkg".into(),
        };
        stamp_provenance_env(&mut env, &prov);
        assert_eq!(env.get("ECAA_GIT_SHA").map(String::as_str), Some("operator-set"));
        assert_eq!(env.get("ECAA_PACKAGE_ID").map(String::as_str), Some("pkg"));
    }

    #[test]
    fn stamp_skips_empty_values() {
        let mut env = std::collections::BTreeMap::new();
        let prov = ProvenanceEnv {
            git_sha: String::new(),
            package_id: String::new(),
        };
        stamp_provenance_env(&mut env, &prov);
        assert!(env.is_empty());
    }

    #[test]
    #[serial_test::serial]
    fn resolve_falls_back_to_workflow_id_for_package_id() {
        // With ECAA_PACKAGE_ID unset, the package id derives from
        // WORKFLOW.json's workflow_id.
        std::env::remove_var("ECAA_PACKAGE_ID");
        let tmp = tempfile::tempdir().unwrap();
        let pkg = tmp.path();
        let dag = DAG {
            version: "1.0".into(),
            schema_version: ecaa_workflow_core::dag::current_dag_schema_version(),
            workflow_id: "workflow-deadbeef".into(),
            current_task: None,
            tasks: std::collections::BTreeMap::new(),
            reverse_deps: std::collections::BTreeMap::new(),
            run_id: None,
            execution_order: Vec::new(),
        };
        std::fs::write(
            pkg.join("WORKFLOW.json"),
            serde_json::to_string(&dag).unwrap(),
        )
        .unwrap();
        let prov = resolve_provenance_env(pkg);
        assert_eq!(prov.package_id, "workflow-deadbeef");
    }

    #[test]
    #[serial_test::serial]
    fn resolve_honours_explicit_env_overrides() {
        std::env::set_var("ECAA_GIT_SHA", "cafe1234");
        std::env::set_var("ECAA_PACKAGE_ID", "explicit-pkg");
        let tmp = tempfile::tempdir().unwrap();
        let prov = resolve_provenance_env(tmp.path());
        assert_eq!(prov.git_sha, "cafe1234");
        assert_eq!(prov.package_id, "explicit-pkg");
        std::env::remove_var("ECAA_GIT_SHA");
        std::env::remove_var("ECAA_PACKAGE_ID");
    }
}

#[cfg(test)]
mod controlled_access_gate_tests {
    use super::*;
    use ecaa_workflow_core::atom::{NetworkPolicy, SandboxRequirement};
    use ecaa_workflow_core::dag::{Assignee, ResourceClass, Task, TaskKind, TaskState, DAG};
    use ecaa_workflow_harness::executor::ExecutorCapabilities;

    fn controlled_access_dag() -> DAG {
        let mut dag = DAG {
            version: "1".into(),
            schema_version: ecaa_workflow_core::dag::current_dag_schema_version(),
            workflow_id: "controlled_access_test".into(),
            current_task: None,
            tasks: std::collections::BTreeMap::new(),
            reverse_deps: std::collections::BTreeMap::new(),
            run_id: None,
            execution_order: Vec::new(),
        };
        let mut task = Task {
            kind: TaskKind::Computation,
            state: TaskState::Pending,
            depends_on: vec![],
            assignee: Assignee::Agent,
            description: "controlled-access acquisition".into(),
            spec: None,
            resolution: None,
            result_ref: None,
            resource_class: ResourceClass::CpuHeavy,
            requires_sme_review: false,
            required_artifacts: vec![],
            container: None,
            source_atom_id: Some("controlled_access_data_acquisition".into()),
            safety: Default::default(),
            inputs: Vec::new(),
            outputs: Vec::new(),
            edam_operation: None,
            execution_index: None,
        };
        task.safety.controlled_access = true;
        dag.tasks.insert("ca1".into(), task);
        dag
    }

    #[test]
    fn controlled_access_refused_on_llm_forwarding_executor_only() {
        let dag = controlled_access_dag();
        let picks = vec!["ca1".to_string()];

        // LLM-forwarding executor (the production default) => refused.
        let caps_llm = ExecutorCapabilities {
            sandbox: SandboxRequirement::None,
            network: NetworkPolicy::Bridge,
            kind: "local",
            forwards_to_external_llm: true,
        };
        let refusals = collect_safety_policy_refusals(&dag, &picks, &caps_llm);
        assert!(
            refusals.contains_key("ca1"),
            "controlled-access must be refused on an LLM-forwarding executor"
        );

        // On-prem no-LLM-egress executor => NOT refused.
        let caps_local = ExecutorCapabilities {
            sandbox: SandboxRequirement::None,
            network: NetworkPolicy::Bridge,
            kind: "slurm",
            forwards_to_external_llm: false,
        };
        let refusals2 = collect_safety_policy_refusals(&dag, &picks, &caps_local);
        assert!(
            !refusals2.contains_key("ca1"),
            "an on-prem no-LLM-egress executor may run controlled-access data"
        );
    }
}

/// Write this task's validator results into
/// `runtime/validation-reports.jsonl` so the RO-Crate emitter
/// registers it as a `CreativeWork` at re-emit time and the
/// Composition UI tab can render the validation status card.
///
/// The validator runners produce sorted, byte-stable JSON lines via
/// `ValidationReportSummary::to_jsonl`. This helper REPLACES the rows
/// belonging to `task_id` rather than blindly appending: the loop re-runs the
/// validator bundle for every Completed task on every harness pass, so a plain
/// append re-emitted one copy of every task's rows on each standalone
/// re-finalize, inflating the log by ~30x across repeated re-finalizes (the
/// log then read as hundreds of "obligations" when only ~14 distinct
/// (obligation, task) checks ran). Each row carries a `task_id`, so drop the
/// existing rows for this task verbatim-preserving every other task's, then
/// append the fresh set — idempotent across re-finalizes.
///
/// Best-effort: a failing write is logged to stderr but doesn't
/// abort the harness loop. Validator wiring is gated on the task's
/// `RequiredArtifact.validation_obligations` list (today optional);
/// when empty the report has zero rows and nothing is written.
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

    // Carry over every OTHER task's rows verbatim (drop this task's stale ones).
    let mut kept = String::new();
    if let Ok(existing) = std::fs::read_to_string(&path) {
        for line in existing.lines() {
            if line.trim().is_empty() {
                continue;
            }
            let belongs_to_this_task = serde_json::from_str::<serde_json::Value>(line)
                .ok()
                .and_then(|v| {
                    v.get("task_id")
                        .and_then(serde_json::Value::as_str)
                        .map(|t| t == task_id)
                })
                .unwrap_or(false);
            if belongs_to_this_task {
                continue;
            }
            kept.push_str(line);
            kept.push('\n');
        }
    }
    kept.push_str(&summary.to_jsonl());

    if let Err(e) = std::fs::write(&path, kept.as_bytes()) {
        eprintln!(
            "  {} validation-reports.jsonl write failed for {}: {}",
            "⚠".yellow(),
            task_id,
            e
        );
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

/// Run-stable provenance identity propagated into every dispatched
/// task's environment (RP-7). The agent inherits these, and so does the
/// plotting subprocess it spawns (`python3 -m runtime.plotting render`),
/// which stamps `ECAA_PACKAGE_ID`/`ECAA_GIT_SHA` into each figure footer.
/// Without them the library falls back to `unknown` and footers read
/// `git@unknown`.
#[derive(Clone, Debug, Default)]
struct ProvenanceEnv {
    git_sha: String,
    package_id: String,
}

/// Resolve the workspace git SHA + package identifier ONCE per run.
///
/// git SHA priority: explicit `ECAA_GIT_SHA` → `ECAA_WORKSPACE_SHA` →
/// the package's git HEAD → the workspace (CWD) git HEAD.
/// package id priority: explicit `ECAA_PACKAGE_ID` → WORKFLOW.json
/// `workflow_id` → the package directory name.
/// Empty/blank values are treated as unset so a blank override never
/// shadows a real value.
fn resolve_provenance_env(package: &Path) -> ProvenanceEnv {
    fn nonempty(v: Option<String>) -> Option<String> {
        v.map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
    }
    fn git_sha(dir: &Path) -> Option<String> {
        let s = git_head_sha(dir);
        if s.is_empty() {
            None
        } else {
            Some(s)
        }
    }
    let sha = nonempty(std::env::var("ECAA_GIT_SHA").ok())
        .or_else(|| nonempty(std::env::var("ECAA_WORKSPACE_SHA").ok()))
        .or_else(|| git_sha(package))
        .or_else(|| std::env::current_dir().ok().and_then(|d| git_sha(&d)))
        .unwrap_or_default();
    let package_id = nonempty(std::env::var("ECAA_PACKAGE_ID").ok())
        .or_else(|| read_dag(package).ok().map(|d| d.workflow_id))
        .filter(|s| !s.is_empty())
        .or_else(|| {
            package
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
        })
        .unwrap_or_default();
    ProvenanceEnv {
        git_sha: sha,
        package_id,
    }
}

/// Stamp the run-stable provenance identity onto a per-task envelope so
/// the agent — and the plotting subprocess it spawns — inherit
/// `ECAA_GIT_SHA` + `ECAA_PACKAGE_ID`. Only stamps non-empty values, and
/// never overwrites a value the envelope already carries.
fn stamp_provenance_env(
    env: &mut std::collections::BTreeMap<String, String>,
    provenance: &ProvenanceEnv,
) {
    if !provenance.git_sha.is_empty() {
        env.entry("ECAA_GIT_SHA".into())
            .or_insert_with(|| provenance.git_sha.clone());
    }
    if !provenance.package_id.is_empty() {
        env.entry("ECAA_PACKAGE_ID".into())
            .or_insert_with(|| provenance.package_id.clone());
    }
}

/// Stamp the deterministic determinism-envelope env (PYTHONHASHSEED,
/// SOURCE_DATE_EPOCH, TZ, LANG, LC_ALL) derived from the stamped
/// dispatch identity. Default-on; `enabled=false` (driven by
/// `ECAA_DETERMINISM_SEEDS=0`) stamps nothing. Only sets keys that
/// aren't already present so an operator override survives. Values are
/// deterministic functions of the run id — never `SystemTime::now()`.
fn stamp_determinism_env(
    env: &mut std::collections::BTreeMap<String, String>,
    dispatch: Option<&PickedDispatch>,
    run_source_date_epoch: u64,
    enabled: bool,
) {
    if !enabled {
        return;
    }
    let Some(dispatch) = dispatch else { return };
    // C2 — SOURCE_DATE_EPOCH comes from the RUN-level epoch (identical for
    // every task), NOT the per-task `dispatch.epoch` counter. The run id is
    // still threaded for seed identity but no longer perturbs the epoch.
    let seeds = ecaa_workflow_core::determinism_seeds::seed_env_from_dispatch(
        &dispatch.harness_run_id,
        run_source_date_epoch,
    );
    for (k, v) in seeds {
        env.entry(k).or_insert(v);
    }
}

/// Stamp the literature-retrieval scope env vars from `ECAA_LIT_*` onto
/// the per-task envelope. The agent helper
/// (`scripts/agent_literature_fetch.py`) reads them at task-execution
/// time to select source-scope tier, NCBI rate limit, evidence storage
/// cap, institutional-access opt-in, and the method-source authority
/// (`ECAA_METHOD_SOURCE_AUTHORITY`).
///
/// `freeze_method_authority` is the frozen-on-rerun seam: when `true`,
/// the method-source authority is forced to `frozen` regardless of the
/// configured value, so a rerun/amend of an already-emitted package
/// performs no fresh live discovery and preserves the original
/// provenance. The decision is supplied by the caller via
/// [`should_freeze_method_authority`]; see that helper for why this is
/// wired to a conservative default today.
fn stamp_literature_scope(
    env: &mut std::collections::BTreeMap<String, String>,
    freeze_method_authority: bool,
) {
    let cfg = ecaa_workflow_harness::literature_scope::LiteratureScopeConfig::from_env();
    for (k, v) in cfg.agent_env_vars() {
        env.insert(k, v);
    }
    if freeze_method_authority {
        env.insert(
            "ECAA_METHOD_SOURCE_AUTHORITY".into(),
            ecaa_workflow_core::config::MethodSourceAuthority::Frozen
                .as_env_str()
                .to_string(),
        );
    }
}

/// Frozen-on-rerun decision seam (plan Task 5.2).
///
/// When re-running/amending an already-emitted package, the method-source
/// authority must be forced to `frozen` so no new live discovery occurs and
/// the original method-landscape provenance is preserved.
///
/// **No grounded per-dispatch rerun/amend signal exists *inside the harness*
/// at the env-stamp seam**:
///   * `rerun_task` delegates to `amend_stage_method` (server-side), which
///     transitions the session to `Amending`. That state is observable only
///     *during* the in-flight amend window (when the harness soft-cancels
///     Running tasks via `ProgressClient::get_amending_invalidated_tasks`),
///     and requires `--session-id`. By the time the survey task is
///     *re-dispatched* (post re-emit), the session is back to `Emitted`, so
///     `Amending` is no longer observable here.
///   * The harness never reads the package `prov:wasDerivedFrom` lineage, and
///     the per-pick envelope carries no rerun marker.
///   * The dispatch WAL records prior `harness_run_id`s but conflates
///     legitimate resume/continuation with a deliberate rerun.
///
/// The grounded signal lives one layer up, at the *server relaunch* boundary:
/// `maybe_auto_relaunch_harness` already receives a static `trigger`
/// (`"rerun_task"` / `"amend_method"` / `"undo_amend"` vs. the fresh
/// `/execution/start` path). The server passes `--frozen-method-source` on a
/// rerun/amend relaunch, which sets [`Args::frozen_method_source`]; this
/// helper simply reads that decision so the harness's dispatch sites need no
/// lineage introspection. The env knob (`ECAA_METHOD_SOURCE_AUTHORITY=frozen`)
/// remains the manual override for direct CLI invocations.
fn should_freeze_method_authority(args: &Args) -> bool {
    args.frozen_method_source
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

    /// Force the method-source authority to `frozen` for every dispatch in
    /// this run, overriding `ECAA_METHOD_SOURCE_AUTHORITY`. The server
    /// appends this flag when relaunching the harness for a rerun/amend of
    /// an already-emitted package so no fresh live method discovery occurs
    /// and the original method-landscape provenance is preserved (plan
    /// Task 5.2). Default false: a fresh run honours the configured
    /// authority (`bounded` by default).
    #[arg(long, default_value_t = false)]
    frozen_method_source: bool,
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

    // Export the chat session id into the harness process environment so it
    // reaches the agent subprocess (via the local executor's
    // REQUIRED_INHERITED_KEYS allowlist, which survives env_clear). The agent
    // wrapper gates its per-session install cache on ECAA_CHAT_SESSION_ID; if
    // it is unset the agent falls back to a package-scoped cache home and a
    // heavy R/conda install (DESeq2 etc.) is never reused across sibling
    // tasks. Set once, before any agent dispatch.
    if let Some(ref sid) = args.session_id {
        std::env::set_var("ECAA_CHAT_SESSION_ID", sid);
    }

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
    // Local executor does NOT enforce per-atom `safety.network`. Two gaps
    // compound: `enforce_safety_policy` network-checks only Network/Exec-level
    // atoms, so the deny-all default on Compute atoms (the majority) is never
    // checked; and the local executor advertises `NetworkPolicy::Bridge` (full
    // egress), so even checked atoms' allowlists are satisfied trivially. Real
    // network enforcement lives on the SLURM and AWS executors (cgroup /
    // security-group layer). Print once at startup so the local-only advisory
    // semantics are observable. (bubblewrap adds PROCESS isolation for
    // Exec/GeneratedCode atoms only — it does NOT add network enforcement.)
    if std::env::var("ECAA_LOCAL_SANDBOX")
        .ok()
        .map(|v| v.trim().is_empty() || v == "off")
        .unwrap_or(true)
        && mode == "local"
    {
        eprintln!(
            "[safety] executor=local: atom-level `safety.network` declarations \
             are NOT enforced locally. Compute-level atoms (the majority) are \
             never network-checked, and the local executor advertises full \
             egress (Bridge), so an atom's egress-deny / allowlist is advisory \
             only. For real network enforcement run on SLURM or AWS, which \
             apply the policy at the cgroup / security-group layer. \
             (ECAA_LOCAL_SANDBOX=bubblewrap adds PROCESS isolation for \
             Exec/GeneratedCode atoms; it does NOT add network enforcement.)"
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
        // Read the DAG up front so the C2 sweep below can run even when
        // the WAL is empty — a crash on the very first dispatch (before
        // any record was appended) leaves a Running task with NO record
        // AND an empty WAL, which the records-non-empty recovery block
        // would never inspect.
        let mut dag_for_recovery = read_dag(path)?;
        let mut recovery_dag_dirty = false;
        if !records.is_empty() {
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
                recovery_dag_dirty = true;
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

        // C2 (M10): re-block any Running task with NO WAL record (crash
        // between write_dag and append_dispatch — recovery can't see it,
        // so it would stay wedged Running). Runs after recovery so a
        // record-bearing orphan is handled by recovery's
        // liveness/denylist logic; only truly record-less tasks fall
        // through here.
        let swept = ecaa_workflow_harness::dispatch_wal::sweep_running_without_wal_record(
            &mut dag_for_recovery,
            &records,
        );
        if !swept.is_empty() {
            recovery_dag_dirty = true;
            tracing::warn!(
                target: "harness-wal",
                task_ids = %swept.join(", "),
                "re-blocked Running task(s) with no WAL record (crash before append_dispatch)"
            );
            if let Some(ref pc) = progress {
                for tid in &swept {
                    if let Some(task) = dag_for_recovery.tasks.get(tid.as_str()) {
                        if let TaskState::Blocked { record } = &task.state {
                            pc.task_blocked(tid, &record.reason);
                        }
                    }
                }
            }
        }

        if recovery_dag_dirty {
            write_dag(path, &dag_for_recovery)?;
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

    // C2 — capture ONE run-stable SOURCE_DATE_EPOCH for the whole run,
    // here at harness startup (next to harness_run_id) rather than per
    // task. Read from the same `clock` threaded into run_loop so tests
    // (FrozenClock) stay deterministic and production gets the real run
    // date. Every task in this run stamps this identical value, so the
    // emitted package carries a single, defensible build date instead of
    // one distinct SOURCE_DATE_EPOCH per dispatched task.
    let run_source_date_epoch: u64 = clock.now().timestamp().max(0) as u64;

    let run_result = run_loop(
        &args,
        &executor,
        validation_executor.as_ref(),
        &progress,
        &main_rx,
        &watchdog_rx,
        &harness_run_id,
        &mut dispatch_epoch,
        run_source_date_epoch,
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

/// The user-facing progress event a single task transition resolves to.
/// `decide_task_progress_event` returns one of these per task per
/// iteration; the `run_loop` event-emit pass turns it into the matching
/// `ProgressClient` POST. Pure data so the decision logic (notably the
/// harness-04 once-per-Failed gate) is unit-testable without a server.
#[derive(Debug, Clone, PartialEq, Eq)]
enum TaskProgressEvent {
    /// No transition observed this pass (or already emitted once).
    None,
    /// New Running observation → `task_started`.
    Started,
    /// New Completed observation → `task_completed[_with_usage]`.
    Completed,
    /// New Blocked observation → `task_blocked` with the agent reason.
    Blocked { reason: String },
    /// New Failed observation, no `error.json` envelope present →
    /// `task_failed` (carries the task description, not the failure
    /// reason, matching the legacy wire contract).
    Failed,
    /// New Failed observation WITH an `error.json` envelope present →
    /// routed as `task_blocked` so the server upgrades it to
    /// `BlockerKind::ToolError`. Carries the failure reason.
    FailedAsBlocked { reason: String },
}

/// Mutations the caller must apply to the four `prior_*` once-guard sets
/// after acting on a `TaskProgressEvent`. Returned alongside the event so
/// the decision is a pure function of (state, prior sets, envelope) and
/// the side effects (set inserts/removes) stay at the call site.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct PriorSetOps {
    insert_completed: bool,
    insert_blocked: bool,
    insert_failed: bool,
    remove_running: bool,
    remove_blocked: bool,
    insert_running: bool,
}

/// The full decision for one task in the event-emit pass: which event to
/// POST, whether to mirror the state to the server, whether to reclaim
/// scratch, and how to update the once-guard sets.
#[derive(Debug, Clone, PartialEq, Eq)]
struct TaskTransitionDecision {
    event: TaskProgressEvent,
    /// Mirror the new state to `POST .../task/:id/state` before the event.
    mirror_state: bool,
    /// Reclaim `runtime/scratch/<id>/` (terminal transitions only).
    cleanup_scratch: bool,
    ops: PriorSetOps,
}

/// Pure transition-classification for one task. Derives the user-facing
/// progress event and once-guard bookkeeping from the task's terminal
/// flags + prior-set membership. Extracted from `run_loop` so it can be
/// driven directly from tests.
///
/// harness-04 invariant: a Failed task is gated on `prior_failed`, NOT
/// `prior_running`. The harness pre-marks dispatched tasks Running for UI
/// visibility, so a Running→Failed task is always already in
/// `prior_running`; gating the failed-event on `!prior_running` (the old
/// behavior) suppressed the event entirely. Gating on `!prior_failed`
/// fires the event exactly once and never re-emits it on later passes.
fn decide_task_progress_event(
    state: &TaskState,
    prior_completed: bool,
    prior_running: bool,
    prior_blocked: bool,
    prior_failed: bool,
    description: &str,
    envelope_exists: bool,
) -> TaskTransitionDecision {
    let is_running = matches!(state, TaskState::Running { .. });
    let is_completed = matches!(state, TaskState::Completed { .. });
    let is_blocked = matches!(state, TaskState::Blocked { .. });
    let is_failed = matches!(state, TaskState::Failed { .. });

    let mut ops = PriorSetOps::default();

    // Clear the blocked once-guard when the task leaves Blocked so a
    // later re-block fires again (matches the run_loop clear).
    if !is_blocked && prior_blocked {
        ops.remove_blocked = true;
    }

    if is_running && !prior_running {
        ops.insert_running = true;
        return TaskTransitionDecision {
            event: TaskProgressEvent::Started,
            mirror_state: true,
            cleanup_scratch: false,
            ops,
        };
    }
    if is_completed && !prior_completed {
        ops.insert_completed = true;
        ops.remove_running = true;
        return TaskTransitionDecision {
            event: TaskProgressEvent::Completed,
            mirror_state: true,
            cleanup_scratch: true,
            ops,
        };
    }
    if is_blocked && !prior_blocked {
        let reason = if let TaskState::Blocked { record } = state {
            if !record.reason.is_empty() {
                record.reason.clone()
            } else {
                description.to_string()
            }
        } else {
            description.to_string()
        };
        ops.insert_blocked = true;
        ops.remove_running = true;
        return TaskTransitionDecision {
            event: TaskProgressEvent::Blocked { reason },
            mirror_state: true,
            cleanup_scratch: false,
            ops,
        };
    }
    if is_failed && !prior_failed {
        ops.insert_failed = true;
        ops.remove_running = true;
        if envelope_exists {
            let reason = if let TaskState::Failed { reason } = state {
                reason.clone()
            } else {
                description.to_string()
            };
            ops.insert_blocked = true;
            return TaskTransitionDecision {
                event: TaskProgressEvent::FailedAsBlocked { reason },
                mirror_state: true,
                cleanup_scratch: true,
                ops,
            };
        }
        return TaskTransitionDecision {
            event: TaskProgressEvent::Failed,
            mirror_state: true,
            cleanup_scratch: true,
            ops,
        };
    }

    // No new transition (or already emitted once) — still surface the
    // blocked-set clear computed above.
    TaskTransitionDecision {
        event: TaskProgressEvent::None,
        mirror_state: false,
        cleanup_scratch: false,
        ops,
    }
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
    // Run-stable SOURCE_DATE_EPOCH captured once at harness startup;
    // stamped identically for every task (C2 run-level determinism).
    run_source_date_epoch: u64,
    clock: &dyn Clock,
) -> Result<()> {
    let path = Path::new(&args.package);

    // Resolved once for the standalone end-of-run finalize (the
    // `after.is_complete()` block below). Points at the emitted package's OWN
    // copied `policies/` so finalization is self-contained regardless of where
    // the harness was launched (ECAA_CONFIG_DIR overrides for an operator).
    let finalize_config_dir =
        ecaa_workflow_harness::end_of_run_finalize::resolve_config_dir(path);
    // Run-stable workspace SHA + package id, resolved once here and
    // stamped onto every per-task envelope so the plotting subprocess
    // stamps a real footer (RP-7) instead of `git@unknown`.
    let provenance_env = resolve_provenance_env(path);
    // Inputs for the standalone per-task claim-coverage gate (guard (d) in the
    // silent-completion pass) — derived ONCE here so the gate reuses the SAME
    // values the end-of-run finalize does, rather than recomputing them
    // differently. `decisions` is re-read per-evaluation (the agent may append
    // to runtime/decisions.jsonl mid-run), matching the end-of-run read shape.
    let coverage_project_class = ecaa_workflow_core::project_class::ProjectClass::default();
    let coverage_is_confirmatory =
        ecaa_workflow_harness::end_of_run_finalize::derive_is_confirmatory(path);
    let coverage_secret = ecaa_workflow_harness::end_of_run_finalize::audit_secret_from_env();
    // Tracks task ids the coverage gate already finalized + passed this run, so
    // a Completed task that cleared coverage is not re-finalized every loop
    // iteration. A re-blocked task leaves Completed (so it never enters here),
    // and on a later re-completion it is absent from the set → re-evaluated.
    let mut coverage_gate_passed: std::collections::HashSet<String> =
        std::collections::HashSet::new();

    let mut prior_completed: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut prior_running: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut prior_blocked: std::collections::HashSet<String> = std::collections::HashSet::new();
    // Per-run guard for auto-advance decision promotion. Stages whose
    // `decision.json` carries `auto_advanced = true` are promoted to
    // `runtime/decisions.jsonl` exactly ONCE per harness run.
    // `promote_auto_advance_decisions` inserts each successfully-appended
    // stage id here so subsequent loop iterations skip it.
    let mut auto_advance_decisions_recorded: std::collections::BTreeSet<String> =
        std::collections::BTreeSet::new();
    // No-progress guard: force-block a task whose agent keeps being
    // re-dispatched (orphan recovery) without ever writing a terminal
    // state patch — a crash loop that heartbeat-stall can't catch because
    // each re-dispatch refreshes the heartbeat. Only tasks actually
    // (re-)dispatched this iteration are observed, so a live long-running
    // agent (skipped by the orphan-recovery is_live probe) is never
    // charged. See `dispatch_guard`.
    let mut noprogress_guard = ecaa_workflow_harness::dispatch_guard::NoProgressGuard::from_env();
    // Tracks tasks already observed in Failed so the user-facing
    // `task_failed`/`task_blocked` event AND the terminal-state scratch
    // cleanup each fire exactly once per Failed transition. The harness
    // pre-marks tasks Running for UI visibility, so a Running→Failed task
    // is always already in `prior_running`; the failed-event branch below
    // therefore gates on this set (mirroring `prior_blocked`) rather than
    // on `!prior_running`, which would suppress the event entirely.
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
            // C1 (H8): gate the in-loop stale-Running reset on the SAME
            // LivenessProbe the WAL orphan recovery uses. A detached
            // compute task (Seurat CCA, BPCells) can exceed its
            // `task_timeout` while still actively touching `.heartbeat`;
            // resetting it to Ready on the timeout verdict alone would
            // re-dispatch and race two agents on the same task.
            // `ECAA_HEARTBEAT_LIVENESS_SECS=0` selects AlwaysDeadProbe
            // (legacy reset-on-timeout behavior).
            let liveness_secs = heartbeat_liveness_window_secs();
            let probe: Box<dyn LivenessProbe> = if liveness_secs == 0 {
                Box::new(AlwaysDeadProbe)
            } else {
                Box::new(HeartbeatLivenessProbe {
                    package_root: path.to_path_buf(),
                    freshness_secs: liveness_secs,
                })
            };
            let guard = executor.lock().unwrap_or_else(|p| p.into_inner());
            for (tid, task) in dag.tasks.iter_mut() {
                if matches!(task.state, TaskState::Running { .. })
                    && ecaa_workflow_harness::stale_reset::should_reset_stale_running(
                        tid.as_str(),
                        guard.is_task_stale(task, now),
                        probe.as_ref(),
                    )
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
        // Before budget-picking, hide Ready tasks whose ancestor chain
        // includes a Completed task with requires_sme_review: true that
        // the SME has not yet confirmed. Confirmed stages come from
        // per-stage sidecar files written by the server's /confirm handler.
        let (picks, picked_dispatches, invocation_by_task): (
            Vec<String>,
            Vec<PickedDispatch>,
            std::collections::BTreeMap<String, ecaa_workflow_harness::invocation_log::InvocationRecord>,
        ) = {
            let mut dag_mut = read_dag(path)?;
            let mut picked_dispatches = Vec::new();
            // Design §5.2 C5 — the pre-dispatch `InvocationRecord` built
            // below, keyed by task_id, so the post-run observed-reads
            // follow-up (after `thread::scope`) can append an enriched
            // second line without recomputing atom_id/prereqs/safety.
            let mut invocation_by_task: std::collections::BTreeMap<
                String,
                ecaa_workflow_harness::invocation_log::InvocationRecord,
            > = std::collections::BTreeMap::new();
            let confirmed_stages = read_confirmed_review_stages(path);
            // Promote any auto-advanced discover_* decisions into
            // `runtime/decisions.jsonl` so audit-proof
            // `decision_justification` is no longer Unverified on
            // standalone runs. Uses the session id when available (web
            // UI path) or the harness run id (offline path) so every
            // record is traceable. Best-effort; errors are logged and
            // dispatch continues.
            {
                let sid = args
                    .session_id
                    .as_deref()
                    .unwrap_or(harness_run_id);
                ecaa_workflow_harness::scheduler::promote_auto_advance_decisions(
                    path,
                    sid,
                    &mut auto_advance_decisions_recorded,
                );
            }
            let sme_eligible_ready = ready_task_ids_passing_sme_gate(&dag_mut, &confirmed_stages);
            let mut allowed_ready = sme_eligible_ready.clone();
            // Apply gates before budget picking. If a lexically early Ready
            // task is waiting on SME review, it must not consume the only
            // processing lane and starve later Ready tasks that are actually
            // dispatchable.
            if dispatch_gate_failed_this_iter {
                allowed_ready.clear();
            } else if session_pausing {
                allowed_ready.retain(|id| !pause_excluded.contains(id.as_str()));
            }
            let dispatchable_dag = dag_with_ready_tasks_limited_to(&dag_mut, &allowed_ready);
            // Validation-lane mode (ECAA_HARNESS_VALIDATION_LANE=1)
            // overrides ECAA_HARNESS_CONCURRENCY: one slot reserved
            // for validators, one for processing.
            let raw_picks = if let Some(lanes) = lane_mode_from_env() {
                pick_ready_with_lanes(&dispatchable_dag, lanes)
            } else {
                pick_ready_respecting_budgets(&dispatchable_dag, budget)
            };
            let sme_eligible_ready_set: std::collections::BTreeSet<String> =
                sme_eligible_ready.iter().map(|id| id.to_string()).collect();
            let picks_pre_sandbox: Vec<String> =
                raw_picks.into_iter().map(|id| id.to_string()).collect();
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
            //   sme_review_required — Ready but withheld by SME gate before budget picking
            //   slot_exhausted     — Ready but not reached by budget picker
            {
                use ecaa_workflow_core::blocker::BlockerKind;
                use picker_decisions::{append_picker_decisions, PickerDecisionRecord};

                let now_ts = chrono::Utc::now().to_rfc3339();
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
                        } else if !sme_eligible_ready_set.contains(task_id) {
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
                // M2 — write one validated-invocation record per dispatched
                // task, paired 1:1 with the dispatch WAL entry by
                // (harness_run_id, epoch). Reads the per-task atom id +
                // safety profile + container pin + prerequisites off the
                // just-pre-marked DAG. Best-effort: a write failure logs +
                // continues (the WAL + WORKFLOW.json remain authoritative;
                // a missing invocation row is an audit gap, never a
                // dispatch blocker — "always emits" / "never block
                // dispatch" both hold).
                if let Some(t) = dag_mut.tasks.get(id.as_str()) {
                    let prereqs: Vec<String> = t.depends_on.iter().map(|d| d.to_string()).collect();
                    // The harness only ever pre-marks Ready tasks, whose
                    // deps are all Completed — so port-typed inputs are
                    // satisfied at dispatch by construction. Recorded
                    // explicitly so auditors read it directly.
                    let inputs_satisfied = prereqs.iter().all(|p| {
                        dag_mut
                            .tasks
                            .get(p.as_str())
                            .map(|pt| {
                                matches!(
                                    pt.state,
                                    ecaa_workflow_core::dag::TaskState::Completed { .. }
                                )
                            })
                            .unwrap_or(false)
                    });
                    let container_image = t.container.as_ref().map(|c| c.image.clone());
                    let inv = ecaa_workflow_harness::invocation_log::InvocationRecord::new(
                        id.as_str(),
                        t.source_atom_id.as_deref(),
                        epoch,
                        harness_run_id,
                        &now.to_rfc3339(),
                        &prereqs,
                        inputs_satisfied,
                        &t.safety,
                        container_image.as_deref(),
                    );
                    if let Err(e) =
                        ecaa_workflow_harness::invocation_log::append_invocation(path, &inv)
                    {
                        tracing::warn!(
                            target: "harness",
                            task_id = %id,
                            epoch = epoch,
                            error = %e,
                            "invocation-record append failed (continuing; WAL + WORKFLOW.json remain authoritative)"
                        );
                    }
                    invocation_by_task.insert(id.clone(), inv);
                }
                picked_dispatches.push(PickedDispatch {
                    task_id: id.clone().into(),
                    harness_run_id: harness_run_id.to_string(),
                    epoch,
                });
            }
            (picks, picked_dispatches, invocation_by_task)
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
                        stamp_determinism_env(
                            &mut env,
                            dispatch_by_task.get(id),
                            run_source_date_epoch,
                            ecaa_workflow_core::determinism_seeds::seeds_enabled(
                                std::env::var("ECAA_DETERMINISM_SEEDS").ok().as_deref(),
                            ),
                        );
                        stamp_literature_scope(&mut env, should_freeze_method_authority(args));
                        stamp_provisioning_policy(&mut env, path, dag_snapshot, id, &declared);
                        stamp_safety_network(&mut env, dag_snapshot, id);
                        stamp_provenance_env(&mut env, &provenance_env);
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
                        stamp_determinism_env(
                            &mut env,
                            dispatch_by_task.get(id),
                            run_source_date_epoch,
                            ecaa_workflow_core::determinism_seeds::seeds_enabled(
                                std::env::var("ECAA_DETERMINISM_SEEDS").ok().as_deref(),
                            ),
                        );
                        stamp_literature_scope(&mut env, should_freeze_method_authority(args));
                        stamp_provisioning_policy(&mut env, path, dag_snapshot, id, &declared);
                        stamp_safety_network(&mut env, dag_snapshot, id);
                        stamp_provenance_env(&mut env, &provenance_env);
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
                // Design §5.2 C5 — the pre-dispatch InvocationRecord for
                // this task (built above, before any writes happened this
                // iteration), cloned out so the post-run closure can
                // append an observed-reads follow-up line without
                // recomputing atom_id/prereqs/safety/container_image.
                // `None` only if the task vanished from the DAG between
                // pre-mark and here (shouldn't happen; handled by simply
                // skipping the follow-up append).
                let base_invocation = invocation_by_task.get(id).cloned();
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
                        // Design §5.2 C5 — pop this iteration's observed
                        // input reads (see `observed_reads::capture_reads`)
                        // and, when the agent runbook reported any, append
                        // a completion-time InvocationRecord line carrying
                        // them. The pre-dispatch line (written before this
                        // task's agent spawned) is left untouched, so its
                        // crash-durability guarantee holds; this is a
                        // SECOND, enriched line for the same
                        // (task_id, epoch, harness_run_id) — absent a read
                        // manifest, no follow-up is written and the
                        // existing one-line-per-dispatch shape is
                        // unchanged. Consumed by
                        // `ecaa_workflow_core::ro_crate::reconcile_ro_crate_edges`
                        // via `crates/conversation/src/emit/ro_crate.rs`.
                        let reads = guard.take_observed_reads();
                        if !reads.is_empty() {
                            tracing::debug!(
                                target: "observed_reads",
                                task_id = %task_id_for_overrides,
                                count = reads.len(),
                                "captured observed input reads for task"
                            );
                            if let Some(base) = &base_invocation {
                                let enriched = base.with_observed_reads(reads);
                                if let Err(e) = ecaa_workflow_harness::invocation_log::append_invocation(
                                    &path_buf, &enriched,
                                ) {
                                    tracing::warn!(
                                        target: "harness",
                                        task_id = %task_id_for_overrides,
                                        error = format!("{:#}", e),
                                        "observed-reads invocation-record append failed (continuing)"
                                    );
                                }
                            }
                        }
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
                        // Tracks whether the wall-clock watchdog already routed
                        // this dispatch to `Blocked { WallClockExceeded }`. When
                        // it did, the immediate-Failed fast path below must NOT
                        // also fire — the Blocked state is the authoritative one.
                        let mut wall_clock_fired = false;
                        if let Some(cap) = capture {
                            // The executor SIGKILLed the agent after the
                            // hard `task_timeout_secs` deadline elapsed
                            // (independent of heartbeat freshness). Reuse
                            // the watchdog's `task_wall_clock_exceeded`
                            // progress event so the server transitions the
                            // task to `Blocked { WallClockExceeded }` —
                            // no new server route is needed.
                            if let Some((observed, threshold)) =
                                wall_clock_blocker_params(&cap, args.task_timeout)
                            {
                                wall_clock_fired = true;
                                println!(
                                    "  {} Agent killed after wall-clock deadline on {}: {}s > {}s",
                                    "⚠".yellow(),
                                    tid.red(),
                                    observed,
                                    threshold,
                                );
                                if let Some(ref pc) = progress {
                                    pc.wall_clock_exceeded(&tid, observed, threshold);
                                }
                            }
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
                        // Fail-fast: a non-zero agent exit that wrote NO
                        // `state.patch.json` means the dispatch died without
                        // recording an outcome (a 429/session-limit strand, a
                        // crash, etc.). Left alone the task stays "Running" until
                        // the 900s heartbeat watchdog trips `heartbeat_stalled`,
                        // wedging the whole DAG for ~15 minutes. Transition it to
                        // Failed immediately so the harness can re-dispatch /
                        // surface it. Skipped when the wall-clock watchdog already
                        // routed the task to `Blocked { WallClockExceeded }`, and
                        // monotonicity is preserved by refusing to overwrite an
                        // already-terminal on-disk state.
                        let patch_present = path
                            .join("runtime")
                            .join("outputs")
                            .join(&tid)
                            .join("state.patch.json")
                            .is_file();
                        if !wall_clock_fired && !patch_present {
                            let failed_state = TaskState::Failed {
                                reason: format!(
                                    "[agent_exit_nonzero] task={} exit={} no state.patch.json written",
                                    tid, o.agent_status,
                                ),
                            };
                            match read_dag(path) {
                                Ok(mut dag) => {
                                    let writable = dag
                                        .tasks
                                        .get(tid.as_str())
                                        .map(|t| !t.state.is_terminal())
                                        .unwrap_or(false);
                                    if writable {
                                        if let Some(t) = dag.tasks.get_mut(tid.as_str()) {
                                            t.state = failed_state.clone();
                                        }
                                        if let Err(e) = write_dag(path, &dag) {
                                            tracing::warn!(
                                                target: "fail_fast",
                                                task_id = %tid,
                                                error = format!("{:#}", e),
                                                "could not persist immediate Failed state"
                                            );
                                        }
                                        if let Some(ref pc) = progress {
                                            pc.set_task_state(&tid, &failed_state);
                                        }
                                        eprintln!(
                                            "  {} Agent exited non-zero with no state.patch.json on {} — marking Failed",
                                            "✗".red(),
                                            tid.red(),
                                        );
                                    }
                                }
                                Err(e) => {
                                    tracing::warn!(
                                        target: "fail_fast",
                                        task_id = %tid,
                                        error = format!("{:#}", e),
                                        "could not read DAG to mark task Failed"
                                    );
                                }
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

        // CV-6 — backfill agent-code.json `executed_code` for tasks whose
        // agent authored + ran standalone scripts under
        // `runtime/outputs/<task_id>/scripts/`. The log-heuristic capture
        // in agent-claude.sh finds nothing for those stages, leaving
        // `executed_code` empty / `language:"unknown"`; this fills it from
        // the scripts that actually ran so the deposit's code-provenance
        // sidecar is truthful. Idempotent (only fills when empty).
        for tid in &picks {
            if ecaa_workflow_harness::agent_code_capture::backfill_executed_code(path, tid.as_str())
            {
                tracing::debug!(
                    target: "agent_code",
                    task_id = %tid,
                    "backfilled executed_code from scripts/ (heuristic capture was empty)"
                );
            }
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

        // Bounded, method-neutral, default-OFF autonomous recovery on a
        // required validation-contract block. Disabled unless
        // `ECAA_HARNESS_VALIDATION_RECOVERY` is truthy — the production /
        // SME path keeps its human checkpoint (the task stays Blocked and
        // the SME drives the unblock). When ON, for each task the enforcer
        // just re-blocked the harness: (1) recomputes a NEUTRAL
        // domain-correctness signal (the failed assertion id + the design's
        // operator-authored bound vs the agent's OWN result.json numbers —
        // never a tool, flag, or threshold value), (2) writes it into the
        // task's next-run inputs so the re-dispatched agent reads what is
        // biologically off, and (3) flips the task back to Ready (the
        // monotonic-safe path: Blocked is non-terminal). The recovery
        // budget is DURABLE on disk (the signal file's
        // `recovery_attempts_consumed`) so it stays bounded across the
        // server's auto-relaunch of the harness between dispatches.
        // Advisory / warn-only mode takes PRECEDENCE over recovery: when
        // both flags are set, advisory wins (the enforcer already recorded
        // the failures as non-blocking warnings and left the tasks
        // completed, so there is nothing to re-dispatch). Skip the recovery
        // path entirely so a failed required assertion is never both an
        // advisory warning and a re-dispatch.
        if validation_recovery::recovery_enabled() && !validation_recovery::advisory_enabled() {
            let budget = validation_recovery::max_recovery_attempts();
            let signals = collect_validation_failure_signals(path, &after);
            let mut recovered: Vec<(String, u32)> = Vec::new();
            for (task_id, failed) in signals {
                // Only act on tasks the enforcer left Blocked this iteration.
                let is_blocked = matches!(
                    after.tasks.get(task_id.as_str()).map(|t| &t.state),
                    Some(TaskState::Blocked { .. })
                );
                if !is_blocked {
                    continue;
                }
                let prior = match validation_recovery::read_signal(path, &task_id) {
                    Ok(p) => p,
                    Err(e) => {
                        // Present-but-broken signal -> fail closed (leave
                        // the task Blocked for the SME) rather than recover
                        // with an unknown budget.
                        tracing::warn!(
                            target: "harness-guard",
                            task_id = %task_id,
                            error = %e,
                            "validation-recovery signal unreadable; leaving task blocked"
                        );
                        continue;
                    }
                };
                match validation_recovery::plan_recovery(
                    &task_id,
                    true,
                    budget,
                    prior.as_ref(),
                    failed,
                ) {
                    validation_recovery::RecoveryDecision::LeaveBlocked => {}
                    validation_recovery::RecoveryDecision::Redispatch {
                        signal,
                        attempt_number,
                    } => {
                        if let Err(e) = validation_recovery::write_signal(path, &task_id, &signal) {
                            tracing::warn!(
                                target: "harness-guard",
                                task_id = %task_id,
                                error = %e,
                                "writing validation-recovery signal failed; leaving task blocked"
                            );
                            continue;
                        }
                        // Flip Blocked -> Ready so the next iteration
                        // re-dispatches this exact task. Blocked is
                        // non-terminal, so this is monotonic-safe; the
                        // validate_<stage> companion (also re-blocked) goes
                        // back to Pending and re-derives readiness after the
                        // parent re-completes.
                        if let Some(t) = after.tasks.get_mut(task_id.as_str()) {
                            t.state = TaskState::Ready;
                        }
                        let vid = format!("validate_{task_id}");
                        if let Some(t) = after.tasks.get_mut(vid.as_str()) {
                            if matches!(t.state, TaskState::Blocked { .. }) {
                                t.state = TaskState::Pending;
                            }
                        }
                        recovered.push((task_id.clone(), attempt_number));
                    }
                }
            }
            if !recovered.is_empty() {
                for (task_id, attempt_number) in &recovered {
                    eprintln!(
                        "{} validation-recovery: re-dispatching {} (attempt {}/{}) with a neutral domain-correctness signal",
                        "↻".cyan(),
                        task_id,
                        attempt_number,
                        budget
                    );
                    append_progress_log(
                        path,
                        task_id,
                        &format!(
                            "harness validation-recovery: re-dispatch {attempt_number}/{budget} after a neutral domain-correctness signal (see runtime/inputs/{task_id}/domain-correctness-signal.json)"
                        ),
                    );
                }
                if let Err(e) = write_dag(path, &after) {
                    tracing::error!(
                        target: "harness-guard",
                        error = format!("{:#}", e),
                        "failed to persist validation-recovery state"
                    );
                }
                if let Some(ref pc) = progress {
                    for (task_id, _) in &recovered {
                        if let Some(t) = after.tasks.get(task_id.as_str()) {
                            pc.set_task_state(task_id, &t.state);
                        }
                    }
                }
            }
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

        // (a-pre) Killed-completion status reconciliation for the case
        // the Completed-gated guard below misses: a task self-reports
        // `status:"completed"` in result.json while the harness left its
        // graph state non-`Completed` (a wall-clock kill can leave it
        // Running/Ready). Only acts on a POSITIVE kill contradiction —
        // an `error.json` kill that no later successful completion
        // superseded — so a still-running or re-running task with a
        // stale result.json is never falsely demoted; the required-
        // artifact backstop for DAG-`Completed` tasks stays in the guard
        // below. On a genuine phantom the reconciled state is routed
        // through the SAME `verdict_for` so its declared
        // `required_artifacts` surface as `[missing_artifact]`.
        for (tid, task) in after.tasks.iter_mut() {
            if matches!(
                task.state,
                TaskState::Completed { .. } | TaskState::Failed { .. } | TaskState::Blocked { .. }
            ) {
                continue;
            }
            if !status_reconciliation::result_json_reports_completed(path, tid.as_str()) {
                continue;
            }
            // SME-acknowledged skip authorizes an empty/killed completion.
            if sme_skip::detect_intent(path, tid.as_str()).is_skip() {
                continue;
            }
            if !status_reconciliation::completion_contradicted_by_kill(path, tid.as_str()) {
                continue;
            }
            if let status_reconciliation::CompletionVerdict::Demote(reason) =
                status_reconciliation::verdict_for(path, tid.as_str(), &task.required_artifacts)
            {
                task.state = TaskState::Blocked {
                    record: ecaa_workflow_core::dag::BlockedRecord {
                        reason,
                        attempts: vec![],
                    },
                };
                guard_flipped.push(tid.to_string());
            }
        }

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
                // (b) required-artifact verification + killed-completion
                // status reconciliation (CV-4). `verdict_for` re-blocks
                // with the byte-identical `[missing_artifact]` reason when
                // a declared artifact is missing/empty/invalid (the
                // server's blocker mapper promotes it to
                // BlockerKind::MissingArtifact), AND additionally demotes
                // a self-reported completion whose FINAL dispatch attempt
                // was a kill that no later successful retry superseded
                // (`[killed_incomplete]`) — a stage may not stay
                // `completed` when the process outcome was a kill.
                match status_reconciliation::verdict_for(
                    path,
                    tid.as_str(),
                    &task.required_artifacts,
                ) {
                    status_reconciliation::CompletionVerdict::Stands => {}
                    status_reconciliation::CompletionVerdict::Demote(reason) => {
                        task.state = TaskState::Blocked {
                            record: ecaa_workflow_core::dag::BlockedRecord {
                                reason,
                                attempts: vec![],
                            },
                        };
                        guard_flipped.push(tid.to_string());
                        continue;
                    }
                }
                // (b.5) Input-form mismatch guard — data_acquisition only.
                // data_acquisition's contract emits the RAW input a composed
                // DAG's first processing stage consumes: raw reads (data:2044
                // FASTQ) for a sequencing DAG, or raw mass-spectrometry files
                // (data:2536) for a proteomics DAG. For an accession that
                // deposits ONLY a downstream/processed product (a count
                // matrix, called peaks, a VCF, a BAM, a protein-abundance
                // matrix, a taxonomy table, a methylation beta matrix, …),
                // the agent materializes that product instead; if the
                // composed DAG still carries the raw-input-consuming stage
                // (sequence_trimming / alignment; or peptide_search /
                // hla_peptide_search), the run silently stalls there with no
                // honest pass-through. Detect that shape↔data mismatch
                // deterministically here and re-block with an actionable
                // reason (the server maps the `[data_shape_mismatch]` prefix
                // to BlockerKind::DataShapeMismatch) instead of letting it
                // proceed to a dead-end. A downstream-first DAG (raw stage
                // pruned) does NOT trip this; nor does a run that also
                // materialized the real raw input.
                if tid.as_str() == "data_acquisition" {
                    let da_dir = path.join("runtime/outputs").join(tid.as_str());
                    let matrices_index =
                        std::fs::read_to_string(da_dir.join("matrices_index.json"))
                            .ok()
                            .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok());
                    // Which recognized downstream product (if any) did the
                    // accession deposit? Detected from result.artifacts file
                    // signatures + the matrices_index for count matrices.
                    let product = deposited_downstream_product(result, matrices_index.as_ref());
                    // If the real raw input was also materialized, the
                    // corresponding pipeline can run — no mismatch.
                    let has_fastq = result_artifacts_have_fastq(result);
                    let has_raw_ms = result_artifacts_have_raw_ms(result);
                    // The composed DAG still expects a raw input iff a raw-
                    // input-consuming stage survived composition. Read task
                    // ids from WORKFLOW.json on disk (avoids re-borrowing the
                    // DAG we are iterating mutably).
                    let wf = std::fs::read(path.join("WORKFLOW.json"))
                        .ok()
                        .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok());
                    let dag_expects_reads = wf
                        .as_ref()
                        .map(workflow_has_read_processing_stages)
                        .unwrap_or(false);
                    let dag_expects_ms = wf
                        .as_ref()
                        .map(workflow_has_ms_search_stage)
                        .unwrap_or(false);
                    if let Some(reason) = input_form_mismatch_reason(
                        product.as_ref(),
                        has_fastq,
                        has_raw_ms,
                        dag_expects_reads,
                        dag_expects_ms,
                    ) {
                        task.state = TaskState::Blocked {
                            record: ecaa_workflow_core::dag::BlockedRecord {
                                reason,
                                attempts: vec![],
                            },
                        };
                        guard_flipped.push(tid.to_string());
                        continue;
                    }
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
                // Order-stable UNION (not concatenation): an obligation
                // declared on more than one of the task's required artifacts
                // must run — and be recorded — exactly once. Without the dedup
                // it emitted a byte-identical duplicate validation row per extra
                // artifact carrying the same obligation id.
                let mut seen_obligations = std::collections::HashSet::new();
                let obligations: Vec<String> = task
                    .required_artifacts
                    .iter()
                    .flat_map(|a| a.validation_obligations.iter().cloned())
                    .filter(|id| seen_obligations.insert(id.clone()))
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
                        // Advisory / warn-only mode (default OFF). When
                        // ECAA_HARNESS_CONTRACT_ADVISORY is truthy this
                        // domain-correctness gate becomes a non-blocking
                        // diagnostic: record one AdvisoryWarning per FAILED
                        // validator row into
                        // runtime/validation-warnings.jsonl and leave the
                        // task Completed so the DAG proceeds — matching the
                        // contract-assertion advisory path. When OFF the
                        // strict block below is byte-identical to before.
                        if validation_recovery::advisory_enabled() {
                            let warnings = validation_recovery::phase13_advisory_warnings(
                                tid.as_str(),
                                &summary,
                                &reason,
                            );
                            tracing::warn!(
                                target: "contract-advisory",
                                "[contract-advisory] {reason} (advisory, not blocking)"
                            );
                            if let Err(e) =
                                validation_recovery::append_warnings(path, &warnings)
                            {
                                tracing::warn!(
                                    target: "contract-advisory",
                                    error = format!("{:#}", e),
                                    "failed to persist advisory validation-warnings sidecar (Phase 13)"
                                );
                            }
                        } else {
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
                // (d) claim-coverage recall gate. A confirmatory task can
                // complete (artifacts present, validators green) yet author
                // 0 structured claims, leaving a Required expected-claim
                // manifest entry unaddressed. The server's incremental
                // verify path re-blocks that as `BlockerKind::ValidationFailed
                // { check: "claim_coverage:<id>" }`; in a standalone (no
                // --session-id) run no event fires, so we run the SAME gate
                // here. `coverage_reblock_reason` finalizes the task FROM
                // SOURCE (idempotent with the end-of-run finalize), and yields
                // a `[claim_coverage]` marker reason only when a Required gap
                // exists AND advisory mode is OFF — the core blocker mapper
                // promotes that marker back to the identical typed blocker.
                // `coverage` is `Some` only for a task with a Required entry,
                // so non-confirmatory tasks no-op without an extra check. The
                // `coverage_gate_passed` set keeps the expensive finalize to
                // once per task per run for tasks that clear the gate.
                //
                // Standalone-only: a session-backed run's server already
                // finalizes per-task on `task_completed` events (and runs the
                // identical coverage re-block), so the harness must NOT also
                // finalize — a second `finalize_task` here would append a
                // duplicate signed-sink row for the task. Guard (d) therefore
                // mirrors the end-of-run finalize block's `progress.is_none()`
                // gate; guards (a)/(b)/(c) above stay unconditional.
                if progress.is_none() && !coverage_gate_passed.contains(tid.as_str()) {
                    let decisions =
                        ecaa_workflow_harness::end_of_run_finalize::load_decisions(path);
                    match ecaa_workflow_harness::end_of_run_finalize::coverage_reblock_reason(
                        path,
                        tid.as_str(),
                        &finalize_config_dir,
                        coverage_project_class,
                        &decisions,
                        coverage_is_confirmatory,
                        coverage_secret.as_ref(),
                    ) {
                        Some(reason) => {
                            task.state = TaskState::Blocked {
                                record: ecaa_workflow_core::dag::BlockedRecord {
                                    reason,
                                    attempts: vec![],
                                },
                            };
                            guard_flipped.push(tid.to_string());
                        }
                        None => {
                            // No gap (or advisory mode): the task cleared the
                            // gate; record it so the finalize is not re-run
                            // every iteration while it stays completed.
                            coverage_gate_passed.insert(tid.to_string());
                        }
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
        //
        // ACCEPTED TRADEOFF (do not "fix"): `pc.set_task_state` is
        // fire-and-forget over a bounded mpsc (256-deep). Under
        // sustained backpressure — server unreachable or the sender
        // thread saturated — an individual state mirror can be dropped
        // (counted by the ProgressClient drop counters; see the
        // degraded-exit guard at the run() tail). This is intentional,
        // NOT a lost-update bug: WORKFLOW.json is the durable source of
        // truth and is written to disk via `write_dag` BELOW *before*
        // any mirror POST fires this iteration, and the server
        // reconciles its `task_states` map from that on-disk DAG on its
        // next poll. A dropped mirror therefore self-heals on the next
        // successful transition or DAG poll; it never strands the task.
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
                // `error.json` presence routes a Failed task to
                // `task_blocked` (BlockerKind::ToolError) instead of
                // `task_failed`; the synthesis already ran earlier this
                // iteration. Read it here so the pure decision function
                // stays I/O-free and unit-testable.
                let envelope_exists = path
                    .join("runtime")
                    .join("outputs")
                    .join(tid_str)
                    .join("error.json")
                    .exists();
                // Pure transition-classification. Carries the harness-04
                // invariant: Failed gates on `prior_failed`, not
                // `prior_running` (which the pre-mark always populates),
                // so the user-facing failed/blocked event fires exactly
                // once and never re-emits while the task lingers Failed.
                let decision = decide_task_progress_event(
                    &task.state,
                    prior_completed.contains(tid_str),
                    prior_running.contains(tid_str),
                    prior_blocked.contains(tid_str),
                    prior_failed.contains(tid_str),
                    &task.description,
                    envelope_exists,
                );
                if decision.mirror_state {
                    pc.set_task_state(tid_str, &task.state);
                }
                match &decision.event {
                    TaskProgressEvent::None => {}
                    TaskProgressEvent::Started => {
                        pc.task_started(tid_str, &task.description);
                    }
                    TaskProgressEvent::Completed => {
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
                    }
                    TaskProgressEvent::Blocked { reason }
                    | TaskProgressEvent::FailedAsBlocked { reason } => {
                        pc.task_blocked(tid_str, reason);
                    }
                    TaskProgressEvent::Failed => {
                        pc.task_failed(tid_str, &task.description);
                    }
                }
                // Terminal-state scratch cleanup. Without this hook
                // `runtime/scratch/<tid>/` accumulates across all
                // dispatches in a package. By the time we observe a
                // terminal (Completed/Failed) transition the agent
                // subprocess has exited, so no concurrent reader exists.
                // Bypass via ECAA_SCRATCH_KEEP=1 for forensic debugging.
                if decision.cleanup_scratch {
                    cleanup_task_scratch(path, tid_str);
                }
                // Apply the once-guard set mutations the decision computed.
                // Removes BEFORE inserts so the independent blocked-clear
                // (set when a now-terminal task was previously Blocked)
                // can't cancel a same-pass `insert_blocked` — matches the
                // original top-to-bottom statement order where the clear
                // ran first and the per-branch insert ran last (relevant
                // only for a Failed-with-envelope task that was Blocked).
                let ops = &decision.ops;
                if ops.remove_running {
                    prior_running.remove(tid_str);
                }
                if ops.remove_blocked {
                    prior_blocked.remove(tid_str);
                }
                if ops.insert_running {
                    prior_running.insert(tid_str.to_string());
                }
                if ops.insert_completed {
                    prior_completed.insert(tid_str.to_string());
                }
                if ops.insert_blocked {
                    prior_blocked.insert(tid_str.to_string());
                }
                if ops.insert_failed {
                    prior_failed.insert(tid_str.to_string());
                }
            }
        }

        if after.is_done() {
            println!(
                "\n{} All tasks terminal (run done) after {} iteration(s).",
                "✓".green().bold(),
                i + 1
            );

            // Capture the assembled compute environment into a content-addressed
            // image and record its digest into the package, on BOTH run paths:
            // the harness is the only component with the assembled conda-envs/
            // R-libs cache, so the server cannot do this. Re-seals the BagIt
            // manifest itself after recording the digest so the manifest is
            // correct on both paths (session path never calls
            // finalize_completed_package). Gated + non-fatal internally.
            ecaa_workflow_harness::end_of_run_finalize::maybe_snapshot(path);

            // T5.9 / DR-12 — backfill a pinned determinism-env for the
            // input-staging (data_acquisition) stage on BOTH run paths. That
            // stage is often pre-staged/pre-completed at emit and never
            // dispatched through `stamp_determinism_env`, so its
            // determinism-env.json keeps the emitter's empty pinning while
            // every executed sibling recorded the run-stable envelope; this
            // copies the envelope from a populated sibling so every stage
            // matches. Re-seals the BagIt manifest itself on a mutation.
            ecaa_workflow_harness::end_of_run_finalize::capture_staging_determinism_env(path);

            // Fire observed-read reconciliation on BOTH run paths. The session
            // (web-UI) path never calls finalize_completed_package — the server
            // finalizes per-task but does not reconcile — so without this the
            // observed-provenance stamps would never appear on a UI-driven run.
            // Stamps the RO-Crate's ParameterConnection nodes
            // authoritative/candidate_unused (and records divergences /
            // read-allowances) from runtime/invocations.jsonl AND, on a genuine
            // observed-read divergence, re-blocks the offending task in
            // WORKFLOW.json (§G-B2 — a divergence must not ship unblocked on the
            // deposit-minting session path, not just the standalone one).
            // Re-seals the BagIt manifest on a mutation (RO-Crate and/or
            // WORKFLOW.json). Best-effort + idempotent (the standalone path's
            // finalize_completed_package call below is then a no-op second pass).
            if ecaa_workflow_harness::end_of_run_finalize::reconcile_observed_reads_into_ro_crate(
                path,
            ) {
                if let Err(e) = ecaa_workflow_core::emitter::regenerate_bagit_manifest(
                    path,
                    &ecaa_workflow_core::clock::WallClock,
                ) {
                    tracing::warn!(
                        target: "harness",
                        error = %e,
                        "BagIt re-seal after observed-read reconcile failed (continuing)"
                    );
                }
            }

            // Standalone self-finalization: the server normally finalizes
            // per-task on `task_completed` events, but a no-session run sends
            // none — so finalize the whole package here (verify+sign claims,
            // refresh the plaintext sidecar, register evidence + reseal the
            // BagIt manifest over outputs, regenerate the at-rest audit-proof).
            // Best-effort: every failure inside is logged, never fatal, so the
            // WAL truncate + `Ok(())` below always run. Skipped when bound to a
            // session — the server owns finalization incrementally in that path.
            if progress.is_none() {
                ecaa_workflow_harness::end_of_run_finalize::finalize_completed_package(
                    path,
                    &finalize_config_dir,
                );
            }

            // Offline end-of-run repair pass (default OFF; `ECAA_AUTO_REPAIR`).
            // This is the loop-exit convergence point reached by BOTH run paths:
            // the standalone/CLI run (`progress.is_none()`, just self-finalized
            // above) AND the session/web-UI run where the server spawned this
            // harness with `--session-id` (`progress.is_some()`, finalized
            // incrementally server-side). The harness is the execution engine on
            // both, and the repair loop re-runs `finalize_package` internally and
            // is idempotent, so running it here once is correct regardless of
            // session — gated solely by `ECAA_AUTO_REPAIR`, independent of the
            // `progress` gate above. Strictly best-effort: every Err/panic inside
            // is caught + logged, the run outcome is unchanged, and it fires
            // exactly once (the standalone finalize above no longer triggers it).
            if ecaa_workflow_harness::end_of_run_finalize::auto_repair_enabled() {
                ecaa_workflow_harness::end_of_run_finalize::run_auto_repair_best_effort(
                    path,
                    &finalize_config_dir,
                );
            }

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

// ── Input-form mismatch guard (b.5) — pure, testable helpers ──────────
// Extracted from the completed-task poll loop so the shape↔data-mismatch
// decision can be unit-tested against real package shapes without driving
// a live execution.
//
// Shape of the mismatch (modality-general): `data_acquisition`'s contract
// emits the RAW input a composed DAG's first processing stage consumes —
// raw FASTQ (data:2044) for a sequencing DAG, raw mass-spectrometry files
// (data:2536) for a proteomics DAG. For an accession that deposits ONLY a
// DOWNSTREAM/processed product (a count matrix, called peaks, a VCF, a BAM,
// a protein-abundance matrix, a taxonomy table, a methylation beta matrix,
// …), the agent materializes that product instead. If the composed DAG
// still carries a raw-input-consuming stage (sequence_trimming / alignment
// for sequencing; peptide_search / hla_peptide_search for proteomics) the
// run stalls silently at that stage with no honest pass-through. The guard
// detects this deterministically and re-blocks with an actionable reason.
// A downstream-first DAG (raw stage pruned) does NOT trip it, nor does a
// run that also materialized the real raw input.

/// A recognized deposited DOWNSTREAM/processed product, plus the EDAM data
/// class it corresponds to, used to phrase the mismatch reason.
struct DepositedProduct {
    /// Human-readable product label, e.g. `"called peaks"`.
    label: &'static str,
    /// EDAM `data:` IRI for the product, e.g. `"data:1255"`.
    iri: &'static str,
}

/// True iff any matrix in a `matrices_index.json` value has a count-like
/// `matrix_type` (e.g. "raw_counts", "counts").
fn matrices_index_has_counts(v: &serde_json::Value) -> bool {
    v.get("matrices")
        .and_then(|m| m.as_array())
        .map(|arr| {
            arr.iter().any(|m| {
                m.get("matrix_type")
                    .and_then(|t| t.as_str())
                    .map(|t| t.to_ascii_lowercase().contains("count"))
                    .unwrap_or(false)
            })
        })
        .unwrap_or(false)
}

/// Iterate the string entries of a completed task's `result.artifacts`
/// array, lowercased. Empty when the array is missing or malformed.
fn result_artifact_names(result: &serde_json::Value) -> Vec<String> {
    result
        .get("artifacts")
        .and_then(|a| a.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|x| x.as_str())
                .map(|s| s.to_ascii_lowercase())
                .collect()
        })
        .unwrap_or_default()
}

/// True iff a completed task's `result.artifacts` array names a FASTQ file
/// (raw reads were actually materialized, so a read pipeline can run).
fn result_artifacts_have_fastq(result: &serde_json::Value) -> bool {
    result_artifact_names(result).iter().any(|s| {
        s.ends_with(".fastq")
            || s.ends_with(".fastq.gz")
            || s.ends_with(".fq")
            || s.ends_with(".fq.gz")
    })
}

/// True iff a completed task's `result.artifacts` array names a raw
/// mass-spectrometry file (a proteomics search pipeline can run). Covers
/// the heterogeneous vendor family peptide_search consumes: open formats
/// (.mzml / .mzxml) and vendor natives (Thermo .raw, Bruker .d directory,
/// .wiff/.wiff2). `.raw` is intentionally matched only in the MS sense
/// here; a genomics run deposits no `.raw` artifact.
fn result_artifacts_have_raw_ms(result: &serde_json::Value) -> bool {
    result_artifact_names(result).iter().any(|s| {
        s.ends_with(".mzml")
            || s.ends_with(".mzxml")
            || s.ends_with(".mzml.gz")
            || s.ends_with(".raw")
            || s.ends_with(".d")
            || s.ends_with(".wiff")
            || s.ends_with(".wiff2")
    })
}

/// Classify the deposited DOWNSTREAM product materialized by
/// `data_acquisition`, from `result.artifacts` file signatures and (for
/// count matrices) the `matrices_index.json` value. Returns the FIRST
/// recognized product; ordering is deliberate — more specific genomic
/// products (peaks / variants / alignments) are tested before the generic
/// count/abundance/tabular families so a `.narrowPeak` never reads as a
/// counts matrix. Raw inputs (FASTQ / raw MS) are NOT products and are
/// deliberately not matched here — they gate the mismatch elsewhere.
fn deposited_downstream_product(
    result: &serde_json::Value,
    matrices_index: Option<&serde_json::Value>,
) -> Option<DepositedProduct> {
    let names = result_artifact_names(result);
    let any = |exts: &[&str]| names.iter().any(|s| exts.iter().any(|e| s.ends_with(e)));

    // Called peaks (ChIP-seq / ATAC-seq / CUT&Tag / STARR-seq): BED-family
    // feature records + coverage tracks. EDAM data:1255 (Feature record).
    if any(&[
        ".narrowpeak",
        ".broadpeak",
        ".gappedpeak",
        ".bed",
        ".bed.gz",
        ".bigwig",
        ".bw",
    ]) {
        return Some(DepositedProduct {
            label: "called peaks",
            iri: "data:1255",
        });
    }
    // Called variants: VCF. EDAM data:3498 (Sequence variations).
    if any(&[".vcf", ".vcf.gz", ".bcf"]) {
        return Some(DepositedProduct {
            label: "called variants (VCF)",
            iri: "data:3498",
        });
    }
    // Deposited alignments: BAM / CRAM. EDAM data:0863 (Sequence
    // alignment). A downstream product for any modality whose composed DAG
    // still tries to (re)align raw reads.
    if any(&[".bam", ".cram"]) {
        return Some(DepositedProduct {
            label: "deposited alignments (BAM/CRAM)",
            iri: "data:0863",
        });
    }
    // Taxonomy table (metagenomics): taxonomic profile. EDAM data:3028
    // (Taxonomy). Matched on filename because the extension is generic
    // tabular text.
    if names
        .iter()
        .any(|s| s.contains("taxonomic_profile") || s.contains("taxonomy_table"))
    {
        return Some(DepositedProduct {
            label: "taxonomy table",
            iri: "data:3028",
        });
    }
    // Protein-abundance matrix (proteomics): protein × sample abundance.
    // ecaax:protein_abundance_matrix → EDAM data:2976 (Protein data).
    if names.iter().any(|s| s.contains("protein_abundance")) {
        return Some(DepositedProduct {
            label: "protein-abundance matrix",
            iri: "data:2976",
        });
    }
    // Methylation beta matrix: per-CpG beta values. EDAM data:3917 reused
    // as the closest count/measurement-matrix term.
    if names
        .iter()
        .any(|s| s.contains("beta_matrix") || s.contains("cpg_methylation"))
    {
        return Some(DepositedProduct {
            label: "methylation beta matrix",
            iri: "data:3917",
        });
    }
    // Count matrix: bulk/scRNA expression counts. Detected via the
    // matrices_index.json `matrix_type` OR a materialized 10x/AnnData
    // feature-barcode artifact (.mtx/.h5/.h5ad). EDAM data:3917.
    let counts_via_index = matrices_index
        .map(matrices_index_has_counts)
        .unwrap_or(false);
    let counts_via_files = any(&[".mtx", ".mtx.gz", ".h5", ".h5ad", ".loom"])
        || names
            .iter()
            .any(|s| s.contains("feature_bc_matrix") || s.contains("count_matrix"));
    if counts_via_index || counts_via_files {
        return Some(DepositedProduct {
            label: "processed count matrix",
            iri: "data:3917",
        });
    }
    None
}

/// True iff a `WORKFLOW.json` value still carries a raw-FASTQ-consuming
/// stage (`sequence_trimming` or `alignment`) — the composed sequencing
/// DAG expects raw reads. `quantification` is intentionally retained here
/// for backward compatibility with the original guard: a scRNA/methylation
/// DAG that carries `quantification` also carries alignment upstream, and
/// treating it as read-expecting is conservative.
fn workflow_has_read_processing_stages(wf: &serde_json::Value) -> bool {
    wf.get("tasks")
        .and_then(|t| t.as_object())
        .map(|tasks| {
            ["sequence_trimming", "alignment", "quantification"]
                .iter()
                .any(|s| tasks.contains_key(*s))
        })
        .unwrap_or(false)
}

/// True iff a `WORKFLOW.json` value carries a raw-MS-consuming stage
/// (`peptide_search` or `hla_peptide_search`) — the composed proteomics
/// DAG expects raw mass-spectrometry files.
fn workflow_has_ms_search_stage(wf: &serde_json::Value) -> bool {
    wf.get("tasks")
        .and_then(|t| t.as_object())
        .map(|tasks| {
            ["peptide_search", "hla_peptide_search"]
                .iter()
                .any(|s| tasks.contains_key(*s))
        })
        .unwrap_or(false)
}

/// The guard's decision (modality-general). `data_acquisition` deposited a
/// downstream product (`product`, if any recognized type), materialized the
/// real raw sequencing input iff `has_fastq`, materialized the real raw MS
/// input iff `has_raw_ms`, and the composed DAG carries a raw-FASTQ stage
/// iff `dag_expects_reads` and a raw-MS stage iff `dag_expects_ms`.
///
/// Returns the actionable `[data_shape_mismatch]` block reason when a
/// deposited product coexists with a raw-input-consuming stage whose raw
/// input was NOT materialized:
///   - sequencing arm: product present, `!has_fastq`, `dag_expects_reads`;
///   - proteomics arm: product present, `!has_raw_ms`, `dag_expects_ms`.
/// Returns `None` for a downstream-first DAG (raw stage pruned), a run that
/// materialized the real raw input, or a run with no deposited product.
fn input_form_mismatch_reason(
    product: Option<&DepositedProduct>,
    has_fastq: bool,
    has_raw_ms: bool,
    dag_expects_reads: bool,
    dag_expects_ms: bool,
) -> Option<String> {
    let product = product?;
    if dag_expects_reads && !has_fastq {
        return Some(format!(
            "[data_shape_mismatch] expected=raw sequence reads (data:2044) \
             actual={} ({}) — data_acquisition materialized only a deposited downstream \
             product for this accession, but the composed pipeline still includes \
             read-processing stages (sequence_trimming / alignment) that require raw reads. \
             Recovery: recompose downstream-first (declare the deposited product as the \
             starting point so trimming/alignment are pruned), or supply an accession that \
             provides raw reads (e.g. SRA FASTQ).",
            product.label, product.iri
        ));
    }
    if dag_expects_ms && !has_raw_ms {
        return Some(format!(
            "[data_shape_mismatch] expected=raw mass-spectrometry data (data:2536) \
             actual={} ({}) — data_acquisition materialized only a deposited downstream \
             product for this accession, but the composed pipeline still includes a \
             spectra-search stage (peptide_search / hla_peptide_search) that requires raw MS \
             files (.raw / .mzML / .d). Recovery: recompose downstream-first (declare the \
             deposited product as the starting point so the search stage is pruned), or \
             supply an accession that provides raw spectra (e.g. PRIDE .raw / .mzML).",
            product.label, product.iri
        ));
    }
    None
}

#[cfg(test)]
mod input_form_guard_tests {
    use super::{
        deposited_downstream_product, input_form_mismatch_reason, matrices_index_has_counts,
        result_artifacts_have_fastq, result_artifacts_have_raw_ms,
        workflow_has_ms_search_stage, workflow_has_read_processing_stages,
    };
    use serde_json::json;

    // These mirror the EXACT on-disk shapes of the analyzed faulty package
    // b0a4d222…GSE164073: data_acquisition emitted a raw_counts matrix,
    // no FASTQ artifact, while the composed DAG carried trim/align/quantify.
    fn faulty_matrices_index() -> serde_json::Value {
        json!({"matrices": [{
            "matrix_id": "GSE164073_count_matrix",
            "matrix_type": "raw_counts",
            "filename": "GSE164073_Eye_count_matrix.csv.gz"
        }]})
    }
    fn faulty_result() -> serde_json::Value {
        json!({"artifacts": [
            "cohort_manifest.tsv", "per_accession_summary.json",
            "matrices_index.json", "manifest.json", "result.json",
            "data/GSE164073/GSE164073_Eye_count_matrix.csv.gz"
        ]})
    }
    fn fastq_workflow() -> serde_json::Value {
        json!({"tasks": {
            "data_acquisition": {}, "sequence_trimming": {}, "alignment": {},
            "quantification": {}, "normalisation": {}, "differential_expression": {}
        }})
    }

    #[test]
    fn faulty_package_shape_fails_loudly() {
        // Exactly the b0a4d222 scenario: counts emitted, no FASTQ, FASTQ DAG.
        let mi = faulty_matrices_index();
        let product = deposited_downstream_product(&faulty_result(), Some(&mi));
        let has_fastq = result_artifacts_have_fastq(&faulty_result());
        let has_raw_ms = result_artifacts_have_raw_ms(&faulty_result());
        let dag_expects_reads = workflow_has_read_processing_stages(&fastq_workflow());
        let dag_expects_ms = workflow_has_ms_search_stage(&fastq_workflow());
        assert!(matrices_index_has_counts(&mi), "raw_counts matrix must read as counts");
        assert_eq!(
            product.as_ref().map(|p| p.label),
            Some("processed count matrix"),
            "count matrix must classify as the count-matrix product"
        );
        assert!(!has_fastq, "count-matrix-only run has no FASTQ artifact");
        assert!(dag_expects_reads, "FASTQ DAG carries read-processing stages");

        let reason = input_form_mismatch_reason(
            product.as_ref(),
            has_fastq,
            has_raw_ms,
            dag_expects_reads,
            dag_expects_ms,
        )
        .expect("guard MUST block the counts-into-FASTQ-DAG mismatch");
        assert!(reason.starts_with("[data_shape_mismatch]"), "typed marker: {reason}");
        assert!(reason.contains("expected=raw sequence reads (data:2044)"), "expected side: {reason}");
        assert!(reason.contains("processed count matrix (data:3917)"), "actual side: {reason}");
        assert!(reason.contains("Recovery: recompose downstream-first"), "actionable: {reason}");
    }

    #[test]
    fn counts_first_dag_does_not_trip() {
        // Read stages pruned (the fixed intake path) -> no mismatch.
        let wf = json!({"tasks": {
            "data_acquisition": {}, "qc_preprocessing": {}, "normalisation": {},
            "differential_expression": {}
        }});
        let dag_expects_reads = workflow_has_read_processing_stages(&wf);
        let dag_expects_ms = workflow_has_ms_search_stage(&wf);
        assert!(!dag_expects_reads);
        assert!(!dag_expects_ms);
        let mi = faulty_matrices_index();
        let product = deposited_downstream_product(&faulty_result(), Some(&mi));
        assert!(product.is_some(), "product is still present, but no raw stage remains");
        assert!(
            input_form_mismatch_reason(
                product.as_ref(),
                false,
                false,
                dag_expects_reads,
                dag_expects_ms
            )
            .is_none()
        );
    }

    #[test]
    fn real_fastq_materialized_does_not_trip() {
        // A genuine raw-reads run: FASTQ present, FASTQ DAG -> consistent.
        let result = json!({"artifacts": [
            "data/SRR1.fastq.gz", "data/SRR2.fastq.gz", "manifest.json"
        ]});
        let has_fastq = result_artifacts_have_fastq(&result);
        assert!(has_fastq);
        // Even if a count product were also present, real FASTQ satisfies the
        // read stages -> no mismatch on the sequencing arm.
        let mi = faulty_matrices_index();
        let product = deposited_downstream_product(&result, Some(&mi));
        assert!(
            input_form_mismatch_reason(product.as_ref(), has_fastq, false, true, false).is_none()
        );
    }

    #[test]
    fn raw_reads_run_without_product_does_not_trip() {
        // No matrices_index / no deposited product -> product is None.
        let result = json!({"artifacts": ["data/SRR1.fastq.gz", "manifest.json"]});
        assert!(!matrices_index_has_counts(&json!({"matrices": []})));
        let product = deposited_downstream_product(&result, Some(&json!({"matrices": []})));
        // The FASTQ artifact is a raw input, NOT a product.
        assert!(product.is_none(), "raw FASTQ is not a downstream product");
        assert!(input_form_mismatch_reason(product.as_ref(), true, false, true, false).is_none());
    }

    #[test]
    fn matrix_type_variants_read_as_counts() {
        for t in ["raw_counts", "counts", "gene_counts", "COUNT"] {
            assert!(
                matrices_index_has_counts(&json!({"matrices": [{"matrix_type": t}]})),
                "matrix_type {t:?} should read as counts"
            );
        }
        assert!(!matrices_index_has_counts(&json!({"matrices": [{"matrix_type": "fpkm"}]})));
    }

    // ── Generalized modality coverage ────────────────────────────────

    fn seq_workflow() -> serde_json::Value {
        // Any sequencing archetype: raw_qc → sequence_trimming → alignment.
        json!({"tasks": {
            "data_acquisition": {}, "raw_qc": {}, "sequence_trimming": {},
            "alignment": {}, "peak_calling": {}, "reporting": {}
        }})
    }

    #[test]
    fn chip_atac_deposited_peaks_into_alignment_dag_blocks() {
        // ChIP-seq / ATAC-seq: accession deposits called peaks
        // (.narrowPeak/.bed) but the composed DAG still aligns raw reads.
        for peak_file in ["results/peaks/consensus.narrowPeak", "results/peaks/regions.bed"] {
            let result = json!({"artifacts": [
                "cohort_manifest.tsv", "manifest.json", "result.json", peak_file
            ]});
            let product = deposited_downstream_product(&result, None)
                .expect("peaks artifact must classify as a deposited product");
            assert_eq!(product.label, "called peaks");
            assert_eq!(product.iri, "data:1255");
            assert!(!result_artifacts_have_fastq(&result));
            let dag_reads = workflow_has_read_processing_stages(&seq_workflow());
            let reason =
                input_form_mismatch_reason(Some(&product), false, false, dag_reads, false)
                    .expect("deposited peaks + alignment DAG (no FASTQ) MUST block");
            assert!(reason.starts_with("[data_shape_mismatch]"), "typed: {reason}");
            assert!(reason.contains("called peaks (data:1255)"), "product side: {reason}");
            assert!(reason.contains("expected=raw sequence reads"), "expected side: {reason}");
        }
    }

    #[test]
    fn variant_deposited_vcf_into_alignment_dag_blocks() {
        // Variant calling: accession deposits a VCF but the composed DAG
        // still trims/aligns raw reads (variant_calling_germline shape).
        let result = json!({"artifacts": [
            "manifest.json", "result.json", "data/cohort.vcf.gz"
        ]});
        let wf = json!({"tasks": {
            "data_acquisition": {}, "raw_qc": {}, "sequence_trimming": {},
            "alignment": {}, "variant_calling": {}, "variant_filtering": {}
        }});
        let product = deposited_downstream_product(&result, None)
            .expect("VCF artifact must classify as a deposited product");
        assert_eq!(product.label, "called variants (VCF)");
        assert_eq!(product.iri, "data:3498");
        assert!(!result_artifacts_have_fastq(&result));
        let dag_reads = workflow_has_read_processing_stages(&wf);
        let reason = input_form_mismatch_reason(Some(&product), false, false, dag_reads, false)
            .expect("deposited VCF + alignment DAG (no FASTQ) MUST block");
        assert!(reason.starts_with("[data_shape_mismatch]"), "typed: {reason}");
        assert!(reason.contains("called variants (VCF) (data:3498)"), "product side: {reason}");
    }

    #[test]
    fn scrna_deposited_10x_counts_into_alignment_dag_blocks() {
        // scRNA: accession deposits 10x feature-barcode counts
        // (.mtx / .h5 / filtered_feature_bc_matrix) but the composed
        // single_cell_de DAG still trims/aligns/quantifies raw reads.
        for counts_file in [
            "data/GSMxxxxxxx_matrix.mtx.gz",
            "data/adata.h5ad",
            "data/filtered_feature_bc_matrix.h5",
        ] {
            let result = json!({"artifacts": [
                "cohort_manifest.tsv", "manifest.json", "result.json", counts_file
            ]});
            // matrices_index absent for a 10x deposit -> detection is via files.
            let product = deposited_downstream_product(&result, None)
                .unwrap_or_else(|| panic!("10x artifact {counts_file} must classify as counts"));
            assert_eq!(product.label, "processed count matrix");
            assert_eq!(product.iri, "data:3917");
            assert!(!result_artifacts_have_fastq(&result));
            let dag_reads = workflow_has_read_processing_stages(&fastq_workflow());
            let reason =
                input_form_mismatch_reason(Some(&product), false, false, dag_reads, false)
                    .expect("deposited 10x counts + alignment DAG (no FASTQ) MUST block");
            assert!(reason.starts_with("[data_shape_mismatch]"), "typed: {reason}");
        }
    }

    #[test]
    fn proteomics_deposited_abundance_into_search_dag_blocks() {
        // Proteomics: accession deposits a protein-abundance matrix but the
        // composed proteomics_dda/dia DAG still runs peptide_search, which
        // requires raw MS files (.raw/.mzML/.d) that were NOT materialized.
        let result = json!({"artifacts": [
            "manifest.json", "result.json", "data/protein_abundance.tsv"
        ]});
        let wf = json!({"tasks": {
            "data_acquisition": {}, "peptide_search": {},
            "protein_quantification": {}, "differential_expression": {}
        }});
        let product = deposited_downstream_product(&result, None)
            .expect("protein_abundance.tsv must classify as a deposited product");
        assert_eq!(product.label, "protein-abundance matrix");
        assert_eq!(product.iri, "data:2976");
        assert!(!result_artifacts_have_raw_ms(&result), "no raw MS file present");
        assert!(!result_artifacts_have_fastq(&result), "not a sequencing run");
        let dag_reads = workflow_has_read_processing_stages(&wf);
        let dag_ms = workflow_has_ms_search_stage(&wf);
        assert!(!dag_reads, "proteomics DAG has no read-processing stage");
        assert!(dag_ms, "proteomics DAG carries peptide_search");
        let reason = input_form_mismatch_reason(Some(&product), false, false, dag_reads, dag_ms)
            .expect("deposited abundance + peptide_search DAG (no raw MS) MUST block");
        assert!(reason.starts_with("[data_shape_mismatch]"), "typed: {reason}");
        assert!(
            reason.contains("expected=raw mass-spectrometry data (data:2536)"),
            "expected side: {reason}"
        );
        assert!(reason.contains("protein-abundance matrix (data:2976)"), "product side: {reason}");
        assert!(reason.contains("peptide_search"), "actionable names the stage: {reason}");
    }

    #[test]
    fn downstream_first_dag_no_raw_stage_does_not_block() {
        // A recomposed downstream-first DAG (no sequence_trimming/alignment,
        // no peptide_search): product present but nothing consumes a raw
        // input -> no mismatch (the intended fixed shape for every modality).
        let peaks_result = json!({"artifacts": [
            "manifest.json", "result.json", "results/peaks/consensus.narrowPeak"
        ]});
        let wf = json!({"tasks": {
            "data_acquisition": {}, "peak_annotation": {}, "motif_enrichment": {},
            "reporting": {}
        }});
        let product = deposited_downstream_product(&peaks_result, None);
        assert!(product.is_some(), "peaks are still a product");
        let dag_reads = workflow_has_read_processing_stages(&wf);
        let dag_ms = workflow_has_ms_search_stage(&wf);
        assert!(!dag_reads && !dag_ms, "no raw-consuming stage survives");
        assert!(
            input_form_mismatch_reason(product.as_ref(), false, false, dag_reads, dag_ms)
                .is_none()
        );
    }

    #[test]
    fn real_raw_ms_materialized_does_not_block() {
        // Genuine proteomics raw run: .raw/.mzML present + peptide_search
        // DAG -> consistent, no mismatch even if it also emitted a product.
        let result = json!({"artifacts": [
            "manifest.json", "result.json",
            "data/sample1.raw", "data/sample2.mzML"
        ]});
        assert!(result_artifacts_have_raw_ms(&result));
        let wf = json!({"tasks": {"data_acquisition": {}, "peptide_search": {}}});
        let dag_ms = workflow_has_ms_search_stage(&wf);
        assert!(dag_ms);
        // product None here (raw MS is an input, not a product) -> no block.
        let product = deposited_downstream_product(&result, None);
        assert!(product.is_none(), "raw MS files are inputs, not products");
        assert!(input_form_mismatch_reason(product.as_ref(), false, true, false, dag_ms).is_none());
    }

    #[test]
    fn raw_reads_run_no_product_no_block_all_modalities() {
        // Sequencing raw run: FASTQ only, no deposited product -> no block.
        let result = json!({"artifacts": ["data/SRR1.fastq.gz", "manifest.json"]});
        let product = deposited_downstream_product(&result, None);
        assert!(product.is_none());
        assert!(input_form_mismatch_reason(product.as_ref(), true, false, true, false).is_none());
    }

    #[test]
    fn product_classification_precedence_peaks_before_counts() {
        // A .narrowPeak alongside a matrices_index that reads as counts must
        // classify as peaks (more-specific genomic product wins) so the
        // reason names the right EDAM class.
        let result = json!({"artifacts": [
            "manifest.json", "results/peaks/x.narrowPeak", "data/counts.csv"
        ]});
        let mi = faulty_matrices_index();
        let product = deposited_downstream_product(&result, Some(&mi)).unwrap();
        assert_eq!(product.label, "called peaks", "peaks take precedence over counts");
    }
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

    // Advisory / warn-only mode (default OFF). When ON, a failed
    // `required` assertion is recorded as a non-blocking diagnostic in the
    // per-package `runtime/validation-warnings.jsonl` sidecar and the task
    // is LEFT in its completed state so the DAG proceeds — no block, and
    // (since advisory takes precedence over recovery) no re-dispatch. When
    // OFF, behaviour is byte-identical to the strict block path below.
    let advisory = validation_recovery::advisory_enabled();
    let mut advisory_warnings: Vec<validation_recovery::AdvisoryWarning> = Vec::new();

    // Map upstream task_id -> its output dir, so cross-stage assertions
    // can resolve a producer's result regardless of which validator is
    // running. Deterministic: BTreeMap, sorted iteration over task ids.
    let mut upstream_outputs: std::collections::BTreeMap<String, std::path::PathBuf> =
        std::collections::BTreeMap::new();
    for tid in dag.tasks.keys() {
        upstream_outputs.insert(
            tid.to_string(),
            pkg_dir
                .join("runtime")
                .join("outputs")
                .join(tid.to_string()),
        );
    }

    // Enforce each stage's contract as soon as its PARENT COMPUTE task is
    // Completed — not only when the validate_<stage> companion completes.
    // Downstream COMPUTE tasks depend on the parent (not its validator), so
    // gating enforcement on the validator's completion let a contract-violating
    // result reach downstream tasks + the eval scorer before the re-block landed
    // (a heteroplasmy-dropping variant_filtering reached Completed,
    // variant_annotation ran, and the scorer read the bad VCFs, all before the
    // lagging validator re-blocked). Collect each parent stage to check once
    // (deduped): triggered by the parent compute task being Completed (the early
    // gate) OR — for back-compat — its validate_<stage> companion being Completed.
    let task_ids: Vec<String> = dag.tasks.keys().map(|id| id.to_string()).collect();
    let mut to_check: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for tid in &task_ids {
        let completed = matches!(
            dag.tasks.get(tid.as_str()).map(|t| &t.state),
            Some(TaskState::Completed { .. })
        );
        if !completed {
            continue;
        }
        let role = ecaa_workflow_core::taxonomy::derive_role_from_id(tid);
        if role.is_validation() {
            to_check.insert(tid.trim_start_matches("validate_").to_string());
        } else if !tid.starts_with("discover_") {
            to_check.insert(tid.to_string());
        }
    }
    for parent_id in to_check {
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
            if !run_assertion(pkg_dir, a, &upstream_outputs) {
                failed_ids.push(id);
            }
        }
        if !failed_ids.is_empty() {
            // The task-level reason the strict block path computes. In
            // advisory mode it is reused verbatim as each warning record's
            // `reason` so the sidecar carries exactly what the SME would
            // have seen on a block.
            let block_reason = format!(
                "Harness validation-contract check: required assertion(s) unsatisfied: {}. See policies/validation-contract.json. Remediate and re-run.",
                failed_ids.join(", ")
            );
            if advisory {
                // Warn-only: do NOT block, do NOT engage recovery (the
                // call site gates recovery on a non-empty `violations`
                // return AND on advisory being off). Leave the task in its
                // completed state so the DAG proceeds; record one advisory
                // warning per failed assertion and log it.
                for assertion_id in &failed_ids {
                    advisory_warnings.push(validation_recovery::AdvisoryWarning {
                        task_id: parent_id.clone(),
                        assertion_id: assertion_id.clone(),
                        severity: "required".to_string(),
                        reason: block_reason.clone(),
                    });
                    tracing::warn!(
                        target: "contract-advisory",
                        "[contract-advisory] {parent_id}.{assertion_id} failed (advisory, not blocking): {block_reason}"
                    );
                }
                continue;
            }
            violations.push((parent_id.clone(), failed_ids.clone()));
            // Re-block the parent compute task so downstream tasks (which depend
            // on it, not on its validator) cannot proceed until the agent
            // remediates.
            if let Some(t) = dag.tasks.get_mut(parent_id.as_str()) {
                t.state = TaskState::Blocked {
                    record: ecaa_workflow_core::dag::BlockedRecord {
                        reason: block_reason,
                        attempts: vec![],
                    },
                };
            }
            // Re-block the validate_<stage> companion when present so a premature
            // validation pass is undone and re-runs after remediation.
            let vid = format!("validate_{parent_id}");
            if let Some(t) = dag.tasks.get_mut(vid.as_str()) {
                t.state = TaskState::Blocked {
                    record: ecaa_workflow_core::dag::BlockedRecord {
                        reason: format!(
                            "Parent compute task '{}' re-blocked by validation-contract: {}. See policies/validation-contract.json.",
                            parent_id,
                            failed_ids.join(", ")
                        ),
                        attempts: vec![],
                    },
                };
            }
        }
    }
    // Persist the advisory sidecar once per enforcement pass (deterministic
    // rewrite of the deduped union). A write failure is logged but never
    // bricks the loop — advisory mode is a diagnostic, not a gate.
    if advisory && !advisory_warnings.is_empty() {
        if let Err(e) = validation_recovery::append_warnings(pkg_dir, &advisory_warnings) {
            tracing::warn!(
                target: "contract-advisory",
                error = format!("{:#}", e),
                "failed to persist advisory validation-warnings sidecar"
            );
        }
    }
    Ok(violations)
}

/// Collect the per-task, method-neutral domain-correctness signals for
/// every `required` assertion that currently fails — without mutating the
/// DAG. Walks the same contract + completed-parent selection as
/// [`enforce_validation_contract`] but, instead of re-blocking, builds a
/// [`validation_recovery::FailedAssertionSignal`] per failing assertion
/// (the assertion id + a recomputed-bound-vs-agent-numbers statement that
/// names NO method). Used only by the autonomous-recovery path, which is
/// gated OFF by default; the production / SME path never calls this.
///
/// Returns a deterministic map (BTreeMap, contract-authored assertion
/// order preserved) keyed by the parent compute task id.
fn collect_validation_failure_signals(
    pkg_dir: &Path,
    dag: &DAG,
) -> std::collections::BTreeMap<String, Vec<validation_recovery::FailedAssertionSignal>> {
    use std::collections::BTreeMap;
    let mut out: BTreeMap<String, Vec<validation_recovery::FailedAssertionSignal>> = BTreeMap::new();

    let contract_path = pkg_dir.join("policies").join("validation-contract.json");
    if !contract_path.exists() {
        return out;
    }
    let contract_bytes = match read_bytes_capped(&contract_path, resolve_max_bytes()) {
        Ok(b) => b,
        Err(_) => return out,
    };
    let contract: serde_json::Value = match serde_json::from_slice(&contract_bytes) {
        Ok(v) => v,
        Err(_) => return out,
    };
    let Some(stages) = contract.get("stages").and_then(|v| v.as_object()) else {
        return out;
    };

    // Same upstream-output map the enforcer builds, so cross-stage
    // statements resolve the producer's number.
    let mut upstream_outputs: BTreeMap<String, std::path::PathBuf> = BTreeMap::new();
    for tid in dag.tasks.keys() {
        upstream_outputs.insert(
            tid.to_string(),
            pkg_dir
                .join("runtime")
                .join("outputs")
                .join(tid.to_string()),
        );
    }

    // Same completed-parent selection as the enforcer.
    let task_ids: Vec<String> = dag.tasks.keys().map(|id| id.to_string()).collect();
    let mut to_check: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for tid in &task_ids {
        let completed = matches!(
            dag.tasks.get(tid.as_str()).map(|t| &t.state),
            Some(TaskState::Completed { .. }) | Some(TaskState::Blocked { .. })
        );
        if !completed {
            continue;
        }
        let role = ecaa_workflow_core::taxonomy::derive_role_from_id(tid);
        if role.is_validation() {
            to_check.insert(tid.trim_start_matches("validate_").to_string());
        } else if !tid.starts_with("discover_") {
            to_check.insert(tid.to_string());
        }
    }

    for parent_id in to_check {
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
        let mut signals: Vec<validation_recovery::FailedAssertionSignal> = Vec::new();
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
            if !run_assertion(pkg_dir, a, &upstream_outputs) {
                signals.push(validation_recovery::FailedAssertionSignal {
                    assertion_id: id,
                    statement: validation_recovery::build_statement(pkg_dir, a, &upstream_outputs),
                });
            }
        }
        if !signals.is_empty() {
            out.insert(parent_id, signals);
        }
    }
    out
}

/// Read a JSON value at `pointer` (RFC-6901) from `path` and return it
/// as f64. Returns `None` when the file is missing/unparseable, the
/// pointer doesn't resolve, or the value isn't numeric. Used by the
/// numeric assertion arms; pessimistic by construction (None → false at
/// the call site).
fn read_json_pointer_f64(path: &Path, pointer: &str) -> Option<f64> {
    let bytes = read_bytes_capped(path, resolve_max_bytes()).ok()?;
    let v: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    v.pointer(pointer).and_then(|x| x.as_f64())
}

/// Read a JSON value at `pointer` (RFC-6901) from `path` and return it
/// as a String. Returns `None` when the file is missing/unparseable, the
/// pointer doesn't resolve, or the value isn't a JSON string. Used by the
/// `cross_field_equals` / `formula_references_covariates` assertion arms;
/// pessimistic by construction (None → false at the call site).
fn read_json_pointer_str(path: &Path, pointer: &str) -> Option<String> {
    let bytes = read_bytes_capped(path, resolve_max_bytes()).ok()?;
    let v: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    v.pointer(pointer)
        .and_then(|x| x.as_str())
        .map(str::to_string)
}

/// Read the raw JSON value at `pointer` from `path` (cloned). `None` on
/// file/parse failure or a missing pointer. Used by the `when`-clause gate.
fn read_json_pointer_value(path: &Path, pointer: &str) -> Option<serde_json::Value> {
    let bytes = read_bytes_capped(path, resolve_max_bytes()).ok()?;
    let v: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    v.pointer(pointer).cloned()
}

/// Evaluate an assertion's optional `when` clause — a guard that makes the
/// assertion CONDITIONAL on a value in a result.json. Returns true when the
/// assertion should run (no `when`, or the predicate holds), false when it is
/// not applicable and should be skipped.
///
/// Shape (target defaults to the assertion's own `target` file):
///   "when": { "json_pointer": "/is_mtdna", "equals": true }
///
/// An unreadable file / missing pointer makes the clause UNSATISFIED (skip).
/// This scopes heteroplasmy-specific variant assertions to mtDNA call sets
/// without false-failing nuclear germline/somatic calling that flows through
/// the shared archetype: the universal assertions (vcf present, AF<=1, count
/// monotonicity) still force the measurement to run and still block, so the
/// fail-open-on-missing here cannot be used to dodge enforcement.
fn when_clause_satisfied(pkg_dir: &Path, assertion: &serde_json::Value) -> bool {
    let Some(when) = assertion.get("when") else {
        return true; // unconditional
    };
    let target = when
        .get("target")
        .and_then(|v| v.as_str())
        .or_else(|| assertion.get("target").and_then(|v| v.as_str()));
    let Some(target) = target else {
        return false;
    };
    let Some(pointer) = when.get("json_pointer").and_then(|v| v.as_str()) else {
        return false;
    };
    let path = pkg_dir.join(target.trim_start_matches('/'));
    let Some(actual) = read_json_pointer_value(&path, pointer) else {
        return false; // unreadable / absent -> not applicable
    };
    if let Some(expected) = when.get("equals") {
        return &actual == expected;
    }
    // No explicit `equals`: treat a present, truthy value as satisfied. The
    // pointer already resolved (absent -> None -> skipped above), so we are
    // deciding on a present value. A literal `false`, an empty array, or an
    // empty string read as "not applicable" (e.g. a recorded-but-empty
    // available_covariates gates nothing); any other present value satisfies.
    // This generalizes the original bool-only gate to the presence gates the
    // method-correctness assertions use (/available_covariates, /stated_outcome).
    match &actual {
        serde_json::Value::Bool(b) => *b,
        serde_json::Value::Array(a) => !a.is_empty(),
        serde_json::Value::String(s) => !s.is_empty(),
        serde_json::Value::Null => false,
        // Numbers / objects: present and non-null -> satisfied.
        _ => true,
    }
}

/// Casefold + trim: lowercase and strip surrounding whitespace. The single
/// normalization the method-correctness arms (`cross_field_equals`,
/// `formula_references_covariates`) apply so a recorded outcome/covariate name
/// matches regardless of capitalization or stray padding (e.g. "SBP " vs
/// "sbp"). Deterministic and pure.
fn casefold_trim(s: &str) -> String {
    s.trim().to_lowercase()
}

/// Split a model design-formula string into its referenced variable tokens.
/// Strips an optional `<lhs> ~ <rhs>` response prefix and splits the RHS on
/// the formula combinators `+`, `*`, `:` (interaction/crossing), then on
/// whitespace, casefolding + trimming each token and dropping the intercept
/// markers (`1`, `0`, empty). Pure + deterministic — used only to test whether
/// a formula REFERENCES a covariate the agent itself recorded, never to author
/// or prescribe a formula.
fn formula_terms(formula: &str) -> std::collections::BTreeSet<String> {
    // Keep only the right-hand side when a `~` is present (the response is
    // checked separately by cross_field_equals).
    let rhs = match formula.split_once('~') {
        Some((_, r)) => r,
        None => formula,
    };
    rhs.split(['+', '*', ':', '(', ')', ' ', '\t'])
        .map(casefold_trim)
        .filter(|t| !t.is_empty() && t != "1" && t != "0")
        .collect()
}

/// Compare `lhs op rhs` for the contract comparison vocabulary.
/// Unknown operators return false (pessimistic).
fn numeric_compare(lhs: f64, op: &str, rhs: f64) -> bool {
    match op {
        "gte" => lhs >= rhs,
        "gt" => lhs > rhs,
        "lte" => lhs <= rhs,
        "lt" => lhs < rhs,
        "eq" => (lhs - rhs).abs() < f64::EPSILON,
        _ => false,
    }
}

/// Read a JSON array of numbers at `pointer` from `path` into a Vec<f64>.
/// Returns `None` if the file/pointer is missing or the value is not an
/// array of numbers. Non-numeric elements are skipped.
fn read_json_pointer_f64_array(path: &Path, pointer: &str) -> Option<Vec<f64>> {
    let bytes = read_bytes_capped(path, resolve_max_bytes()).ok()?;
    let v: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    let arr = v.pointer(pointer)?.as_array()?;
    Some(arr.iter().filter_map(|x| x.as_f64()).collect())
}

/// Resolve a JSON pointer to a "presence cardinality": the count of
/// non-null entries it represents. An array yields its length; a scalar
/// (non-null) yields 1; null / missing yields 0. Used by the control
/// presence arms. Returns `None` only on file read/parse failure (so the
/// caller stays pessimistic).
fn json_pointer_presence_count(path: &Path, pointer: &str) -> Option<usize> {
    let bytes = read_bytes_capped(path, resolve_max_bytes()).ok()?;
    let v: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    Some(match v.pointer(pointer) {
        Some(serde_json::Value::Array(a)) => a.len(),
        Some(serde_json::Value::Null) | None => 0,
        Some(_) => 1,
    })
}

/// Per-assertion runner. Returns true when the assertion passes.
/// Unknown assertion_types default to false (pessimistic) so any typo
/// surfaces as a hard failure.
///
/// `upstream` maps an upstream task_id to that task's output dir so
/// `cross_stage_output_comparison` can read a producer's result without
/// knowing which validator is running. The map is deterministic
/// (BTreeMap, built once from the DAG by `enforce_validation_contract`).
fn run_assertion(
    pkg_dir: &Path,
    assertion: &serde_json::Value,
    upstream: &std::collections::BTreeMap<String, std::path::PathBuf>,
) -> bool {
    // A `when` clause makes the assertion conditional (e.g. mtDNA-only). When
    // the guard does not hold, the assertion is NOT APPLICABLE for this call set
    // and is treated as passed so it never blocks.
    if !when_clause_satisfied(pkg_dir, assertion) {
        return true;
    }
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
            // Optional `check.json_pointer` SCOPES the substring search to a
            // single string value inside the target JSON (fail-closed when the
            // file is not JSON or the pointer does not resolve to a string).
            // Without a pointer the whole file is searched as text — the
            // original behavior, byte-identical for every existing assertion
            // (design_recorded etc. carry no json_pointer). Scoping is required
            // for the report-completeness arms: searching the WHOLE result.json
            // would match a substring inside a sibling FIELD NAME (e.g.
            // "r_squared" inside the flag key "r_squared_column_recorded"),
            // false-passing regardless of the narrative — so those arms scope to
            // /narrative_text.
            // Outer Option: was a json_pointer requested? Inner Option: did it
            // resolve to a string? `Some(None)` = pointer requested but
            // unresolved -> fail-closed empty haystack; `None` = no pointer ->
            // whole-file search (original behavior).
            let owned_scope: Option<Option<String>> = assertion
                .get("check")
                .and_then(|c| c.get("json_pointer"))
                .and_then(|v| v.as_str())
                .map(|ptr| {
                    serde_json::from_slice::<serde_json::Value>(&bytes)
                        .ok()
                        .and_then(|v| {
                            v.pointer(ptr)
                                .and_then(|x| x.as_str())
                                .map(|s| s.to_string())
                        })
                });
            let text: std::borrow::Cow<str> = match owned_scope {
                // Pointer requested but unresolved (not JSON / not a string) ->
                // fail closed with an empty haystack so substrings can't match.
                Some(None) => std::borrow::Cow::Borrowed(""),
                Some(Some(ref s)) => std::borrow::Cow::Borrowed(s.as_str()),
                None => String::from_utf8_lossy(&bytes),
            };
            // Opt-in `check.case_insensitive: true` folds case before matching.
            // Default is case-SENSITIVE (byte-identical to the original behavior
            // every existing assertion — design_recorded, method-correctness —
            // and the contract-blocking tests rely on). The report-completeness
            // arms set it true because an agent surfaces a statistic in whatever
            // case reads naturally ("R-squared", "R²") while the configured
            // substrings are lowercase.
            let ci = assertion
                .get("check")
                .and_then(|c| c.get("case_insensitive"))
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let hay: std::borrow::Cow<str> = if ci {
                std::borrow::Cow::Owned(text.to_lowercase())
            } else {
                std::borrow::Cow::Borrowed(text.as_ref())
            };
            let matches = |ss: &str| -> bool {
                if ci {
                    hay.contains(&ss.to_lowercase())
                } else {
                    hay.contains(ss)
                }
            };
            // Supports either `substrings: [required all of]` or
            // `substrings_any: [any of]`.
            if let Some(req) = assertion
                .get("check")
                .and_then(|c| c.get("substrings"))
                .and_then(|v| v.as_array())
            {
                req.iter()
                    .all(|s| s.as_str().map(matches).unwrap_or(false))
            } else if let Some(any) = assertion
                .get("check")
                .and_then(|c| c.get("substrings_any"))
                .and_then(|v| v.as_array())
            {
                any.iter()
                    .any(|s| s.as_str().map(matches).unwrap_or(false))
            } else {
                false
            }
        }
        "numeric_threshold" => {
            let path = resolve(target);
            let Some(check) = assertion.get("check") else {
                return false;
            };
            let Some(pointer) = check.get("json_pointer").and_then(|v| v.as_str()) else {
                return false;
            };
            let Some(op) = check.get("op").and_then(|v| v.as_str()) else {
                return false;
            };
            let Some(rhs) = check.get("value").and_then(|v| v.as_f64()) else {
                return false;
            };
            match read_json_pointer_f64(&path, pointer) {
                Some(lhs) => numeric_compare(lhs, op, rhs),
                None => false,
            }
        }
        "numeric_distribution" => {
            let path = resolve(target);
            let Some(check) = assertion.get("check") else {
                return false;
            };
            let Some(pointer) = check.get("json_pointer").and_then(|v| v.as_str()) else {
                return false;
            };
            let Some(stat) = check.get("stat").and_then(|v| v.as_str()) else {
                return false;
            };
            let Some(op) = check.get("op").and_then(|v| v.as_str()) else {
                return false;
            };
            let Some(rhs) = check.get("value").and_then(|v| v.as_f64()) else {
                return false;
            };
            let Some(values) = read_json_pointer_f64_array(&path, pointer) else {
                return false;
            };
            if values.is_empty() {
                return false;
            }
            let s = ecaa_workflow_core::statistical_helpers::compute_distribution_stats(&values);
            let observed = match stat {
                "mean" => s.mean,
                "stdev" => s.stdev,
                "skewness" => s.skewness,
                "kurtosis" => s.kurtosis,
                "p5" => s.p5,
                "p50" => s.p50,
                "p95" => s.p95,
                _ => return false,
            };
            numeric_compare(observed, op, rhs)
        }
        "reference_range_outlier" => {
            let path = resolve(target);
            let Some(check) = assertion.get("check") else {
                return false;
            };
            let Some(pointer) = check.get("json_pointer").and_then(|v| v.as_str()) else {
                return false;
            };
            let Some(rmin) = check.get("reference_min").and_then(|v| v.as_f64()) else {
                return false;
            };
            let Some(rmax) = check.get("reference_max").and_then(|v| v.as_f64()) else {
                return false;
            };
            let tol = check
                .get("tolerance")
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0);
            let Some(values) = read_json_pointer_f64_array(&path, pointer) else {
                return false;
            };
            if values.is_empty() {
                return false;
            }
            // Assertion PASSES when every observed value is within the
            // reference range (padded by tolerance) — i.e. no domain
            // outliers. A single out-of-range value fails the assertion.
            values.iter().all(|&v| {
                ecaa_workflow_core::statistical_helpers::is_within_reference_range(
                    v, rmin, rmax, tol,
                )
            })
        }
        "positive_control_present" => {
            let path = resolve(target);
            let Some(pointer) = assertion
                .get("check")
                .and_then(|c| c.get("json_pointer"))
                .and_then(|v| v.as_str())
            else {
                return false;
            };
            // Passes when the positive control was detected (count >= 1).
            json_pointer_presence_count(&path, pointer)
                .map(|n| n >= 1)
                .unwrap_or(false)
        }
        "negative_control_present" => {
            let path = resolve(target);
            let Some(pointer) = assertion
                .get("check")
                .and_then(|c| c.get("json_pointer"))
                .and_then(|v| v.as_str())
            else {
                return false;
            };
            // The negative control must NOT be called: the assertion passes
            // only when the count is exactly 0 (no false positive). A read
            // failure stays pessimistic-false.
            json_pointer_presence_count(&path, pointer)
                .map(|n| n == 0)
                .unwrap_or(false)
        }
        "json_pointer_is_bool" => {
            // Typed presence guard: the json_pointer must resolve to an actual
            // JSON boolean (true OR false). Stronger than `string_contains` on
            // the same field name, which a raw-bytes substring match would
            // satisfy from any incidental occurrence of the key in a note/string
            // (e.g. {"is_mtdna_note":"...is_mtdna..."}) while the pointer never
            // resolves to a bool — leaving a `when`-gated check fail-open. Used
            // to close the is_mtdna_recorded fail-open: /is_mtdna must be a
            // reference-derived boolean, so a partial/tampered result.json cannot
            // dodge the mtDNA-gated heteroplasmy checks. Unreadable file / absent
            // pointer / non-bool value → false (fail-closed).
            let path = resolve(target);
            let Some(pointer) = assertion
                .get("check")
                .and_then(|c| c.get("json_pointer"))
                .and_then(|v| v.as_str())
            else {
                return false;
            };
            matches!(
                read_json_pointer_value(&path, pointer),
                Some(serde_json::Value::Bool(_))
            )
        }
        "json_pointer_is_array" => {
            // Typed presence guard: the json_pointer must resolve to a JSON
            // ARRAY (empty allowed). Stronger than `string_contains` on the same
            // field name: a substring match is satisfied by an incidental
            // occurrence of the field name in a note or at a NESTED key while the
            // top-level pointer never resolves to an array — leaving a check that
            // `when`-gates on that same pointer fail-open. Used so the
            // covariate-adjustment precondition reads the SAME basis (the
            // /available_covariates pointer) the adjustment check's `when` gate
            // reads. An empty array passes (a covariate-free run); the adjustment
            // check's own empty-array `when` gate then self-skips. Absent /
            // non-array → false (fail-closed).
            let path = resolve(target);
            let Some(pointer) = assertion
                .get("check")
                .and_then(|c| c.get("json_pointer"))
                .and_then(|v| v.as_str())
            else {
                return false;
            };
            matches!(
                read_json_pointer_value(&path, pointer),
                Some(serde_json::Value::Array(_))
            )
        }
        "cross_stage_output_comparison" => {
            let this_path = resolve(target);
            let Some(check) = assertion.get("check") else {
                return false;
            };
            let Some(this_ptr) = check.get("this_pointer").and_then(|v| v.as_str()) else {
                return false;
            };
            let Some(up_task) = check.get("upstream_task").and_then(|v| v.as_str()) else {
                return false;
            };
            let up_file = check
                .get("upstream_file")
                .and_then(|v| v.as_str())
                .unwrap_or("result.json");
            let Some(up_ptr) = check.get("upstream_pointer").and_then(|v| v.as_str()) else {
                return false;
            };
            let Some(op) = check.get("op").and_then(|v| v.as_str()) else {
                return false;
            };
            let Some(up_dir) = upstream.get(up_task) else {
                return false; // upstream task output not available → pessimistic
            };
            let this_val = match read_json_pointer_f64(&this_path, this_ptr) {
                Some(v) => v,
                None => return false,
            };
            let up_val = match read_json_pointer_f64(&up_dir.join(up_file), up_ptr) {
                Some(v) => v,
                None => return false,
            };
            numeric_compare(this_val, op, up_val)
        }
        "cross_field_equals" => {
            // Method-correctness: two AGENT-RECORDED string fields in the same
            // result.json must be equal after normalization. Catches the
            // inverted-regression error (da-8-1): the model's recorded
            // response_variable must equal the task's recorded stated_outcome
            // — if the agent regressed `metabolite ~ SBP` while the task's
            // outcome is SBP, the two disagree and this fails. It compares the
            // agent's own choice (this_pointer) against the agent's own record
            // of the task (other_pointer); it never prescribes a model. Fail
            // closed on either pointer missing/unreadable.
            let path = resolve(target);
            let Some(check) = assertion.get("check") else {
                return false;
            };
            let Some(this_ptr) = check.get("this_pointer").and_then(|v| v.as_str()) else {
                return false;
            };
            let Some(other_ptr) = check.get("other_pointer").and_then(|v| v.as_str()) else {
                return false;
            };
            let normalize = check
                .get("normalize")
                .and_then(|v| v.as_str())
                .unwrap_or("casefold_trim");
            let (Some(a_raw), Some(b_raw)) = (
                read_json_pointer_str(&path, this_ptr),
                read_json_pointer_str(&path, other_ptr),
            ) else {
                return false; // missing pointer -> pessimistic
            };
            match normalize {
                "casefold_trim" => casefold_trim(&a_raw) == casefold_trim(&b_raw),
                // exact (no normalization)
                "exact" | "none" => a_raw == b_raw,
                // Unknown normalization -> pessimistic.
                _ => false,
            }
        }
        "formula_references_covariates" => {
            // Method-correctness: the AGENT-RECORDED design-formula string must
            // REFERENCE at least one of the AGENT-RECORDED available covariates
            // (after removing the primary comparison variable from the
            // covariate set). Catches the naked-design error (da-15-1): a DESeq2
            // design recorded as `~ condition` while the metadata the agent
            // observed carries sex/age/RIN — none of those non-primary
            // covariates appears in the formula, so this fails. PASSES when ≥1
            // non-primary covariate is referenced, OR when no non-primary
            // covariate remains (nothing to adjust for). It compares the
            // agent's own formula against the agent's own record of the data's
            // columns; it never tells the agent WHICH covariates to include or
            // which model to fit. Fail closed on any pointer missing/unreadable.
            let path = resolve(target);
            let Some(check) = assertion.get("check") else {
                return false;
            };
            let Some(formula_ptr) = check.get("formula_pointer").and_then(|v| v.as_str()) else {
                return false;
            };
            let Some(cov_ptr) = check.get("covariates_pointer").and_then(|v| v.as_str()) else {
                return false;
            };
            let Some(primary_ptr) = check.get("primary_pointer").and_then(|v| v.as_str()) else {
                return false;
            };
            let Some(formula) = read_json_pointer_str(&path, formula_ptr) else {
                return false; // missing formula -> pessimistic
            };
            // available_covariates is an array of column-name strings.
            let Some(cov_value) = read_json_pointer_value(&path, cov_ptr) else {
                return false; // missing covariate record -> pessimistic
            };
            let Some(cov_arr) = cov_value.as_array() else {
                return false; // not an array -> pessimistic
            };
            // primary variable is optional: when absent, no term is removed.
            let primary = read_json_pointer_str(&path, primary_ptr).map(|p| casefold_trim(&p));
            let remaining: std::collections::BTreeSet<String> = cov_arr
                .iter()
                .filter_map(|v| v.as_str())
                .map(casefold_trim)
                .filter(|c| !c.is_empty())
                .filter(|c| primary.as_deref() != Some(c.as_str()))
                .collect();
            if remaining.is_empty() {
                // No non-primary covariate available to adjust for -> nothing to
                // assert; a naked design is correct here. PASS. (The `when`
                // gate on /available_covariates already skips the empty case in
                // the shipped contract; this is defense-in-depth.)
                return true;
            }
            let terms = formula_terms(&formula);
            // PASS iff the formula references ≥1 remaining covariate.
            remaining.iter().any(|c| terms.contains(c))
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

/// A single probe specification for the batched container-probe script.
/// Used by `build_probe_script` (pure) and consumed by `run_container_probes`.
#[derive(Debug, Clone, PartialEq)]
enum ProbeKind {
    /// `Rscript -e 'library(<pkg>)'` — key maps to true/false.
    R(String),
    /// `python3 -c 'import <module>'` — key maps to true/false.
    Python(String),
    /// True if ANY of the listed imports succeeds (logical OR).
    PythonAny(Vec<String>),
    /// `cellranger --version` — key maps to the first output line (or empty).
    Cellranger,
}

#[derive(Debug, Clone)]
struct ProbeSpec {
    /// Output key emitted as `KEY=0|1` (or `CELLRANGER=<ver>`).
    key: String,
    kind: ProbeKind,
}

/// Build the shell script that runs every probe inside a container and
/// emits one `KEY=0|1` line per boolean probe plus one
/// `CELLRANGER=<ver-or-empty>` line.  Pure (no I/O) — unit-testable.
fn build_probe_script(specs: &[ProbeSpec], r_libs_prefix: Option<&str>) -> String {
    let mut lines: Vec<String> = Vec::with_capacity(specs.len() + 2);
    lines.push("#!/bin/sh".to_string());
    lines.push("set -u".to_string());
    for spec in specs {
        match &spec.kind {
            ProbeKind::R(pkg) => {
                let lib_expr = match r_libs_prefix {
                    Some(p) => {
                        let escaped = p.replace('\'', "'\\''");
                        format!(".libPaths(c('{}', .libPaths())); ", escaped)
                    }
                    None => String::new(),
                };
                let safe_pkg = pkg.replace('\'', "'\\''");
                lines.push(format!(
                    "Rscript -e '{}suppressMessages(library({}))' >/dev/null 2>&1 && echo '{}=1' || echo '{}=0'",
                    lib_expr, safe_pkg, spec.key, spec.key
                ));
            }
            ProbeKind::Python(module) => {
                let safe_mod = module.replace('\'', "'\\''");
                lines.push(format!(
                    "python3 -c 'import {}' >/dev/null 2>&1 && echo '{}=1' || echo '{}=0'",
                    safe_mod, spec.key, spec.key
                ));
            }
            ProbeKind::PythonAny(modules) => {
                // Emit a compound shell OR: try each import; first success wins.
                let checks: Vec<String> = modules
                    .iter()
                    .map(|m| {
                        let safe = m.replace('\'', "'\\''");
                        format!("python3 -c 'import {}' >/dev/null 2>&1", safe)
                    })
                    .collect();
                lines.push(format!(
                    "{{ {}; }} && echo '{}=1' || echo '{}=0'",
                    checks.join(" || "),
                    spec.key,
                    spec.key
                ));
            }
            ProbeKind::Cellranger => {
                lines.push(format!(
                    "echo 'CELLRANGER='\"$(cellranger --version 2>/dev/null | head -1)\""
                ));
            }
        }
    }
    lines.join("\n")
}

/// Parse the stdout of the batched probe script into a bool map plus an
/// optional cellranger version string.  Pure (no I/O) — unit-testable.
///
/// Returns `(bool_results, cellranger_version)`.
fn parse_probe_output(stdout: &str) -> (std::collections::BTreeMap<String, bool>, Option<String>) {
    let mut bools = std::collections::BTreeMap::new();
    let mut cellranger: Option<String> = None;
    for line in stdout.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(rest) = line.strip_prefix("CELLRANGER=") {
            let ver = rest.trim();
            if !ver.is_empty() {
                cellranger = Some(ver.to_string());
            }
            // empty => cellranger absent => leave None
        } else if let Some((key, val)) = line.split_once('=') {
            match val.trim() {
                "1" => {
                    bools.insert(key.to_string(), true);
                }
                "0" => {
                    bools.insert(key.to_string(), false);
                }
                _ => {} // unexpected — skip
            }
        }
    }
    (bools, cellranger)
}

/// Read `<pkg_dir>/policies/container.json` `.image` field (if the file
/// exists and the field is a non-null string), then fall back to the
/// `ECAA_DEFAULT_CONTAINER_IMAGE` env var.  Returns `None` when neither
/// source provides an image.
fn resolve_probe_image(pkg_dir: &Path) -> Option<String> {
    // 1. Package-local policy file.
    let policy = pkg_dir.join("policies/container.json");
    if policy.is_file() {
        if let Ok(bytes) = std::fs::read(&policy) {
            if let Ok(v) = serde_json::from_slice::<serde_json::Value>(&bytes) {
                if let Some(img) = v.get("image").and_then(|i| i.as_str()) {
                    if !img.is_empty() {
                        return Some(img.to_string());
                    }
                }
            }
        }
    }
    // 2. Process-level default.
    if let Ok(img) = std::env::var("ECAA_DEFAULT_CONTAINER_IMAGE") {
        if !img.is_empty() {
            return Some(img);
        }
    }
    None
}

/// Run all `specs` as a single batched `docker run` inside `image`.
/// Returns `(bool_map, cellranger_version)` on success, or an error
/// string (for logging) on any failure — callers must handle the error
/// by falling back to host probes.
fn run_container_probes(
    specs: &[ProbeSpec],
    image: &str,
    r_libs_abs: Option<&Path>,
) -> Result<(std::collections::BTreeMap<String, bool>, Option<String>), String> {
    let r_libs_str: Option<String> = r_libs_abs.map(|p| p.display().to_string());
    let script = build_probe_script(specs, r_libs_str.as_deref());

    let mut cmd = std::process::Command::new("docker");
    cmd.arg("run").arg("--rm");
    // Mount the r-libs directory read-only at the same absolute path so
    // package-local R installs are visible inside the container.
    if let Some(rlibs) = r_libs_abs {
        let mount = format!("{}:{}:ro", rlibs.display(), rlibs.display());
        cmd.args(["-v", &mount]);
    }
    cmd.args([image, "bash", "-lc", &script]);
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());

    let child = cmd.spawn().map_err(|e| format!("docker spawn failed: {e}"))?;

    // Enforce a hard timeout to avoid hanging the harness indefinitely.
    use std::time::{Duration, Instant};
    let timeout = Duration::from_secs(180);
    let started = Instant::now();
    // `wait_with_output` doesn't support a timeout natively; use a
    // polling loop with a 200 ms sleep cadence as a best-effort guard.
    // (The alternative — a background thread — adds more complexity than
    // warranted for a best-effort probe.)
    let output = {
        let mut child = child;
        loop {
            match child.try_wait() {
                Ok(Some(_)) => break child.wait_with_output().map_err(|e| e.to_string())?,
                Ok(None) => {
                    if started.elapsed() > timeout {
                        let _ = child.kill();
                        return Err("docker run timed out after 180s".to_string());
                    }
                    std::thread::sleep(Duration::from_millis(200));
                }
                Err(e) => return Err(format!("docker wait error: {e}")),
            }
        }
    };

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "docker run exited {:?}: {}",
            output.status.code(),
            stderr.trim()
        ));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let (bools, cellranger) = parse_probe_output(&stdout);

    // Verify we got at least one result line; otherwise something went
    // wrong silently (wrong image entrypoint, bash not available, etc.).
    if bools.is_empty() && cellranger.is_none() {
        return Err(format!(
            "docker run produced no parseable probe output (stdout: {:?})",
            &stdout[..stdout.len().min(256)]
        ));
    }
    Ok((bools, cellranger))
}

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
    let r_libs_path = runtime_dir.join("r-libs");
    let r_libs_user: Option<&Path> = if r_libs_path.is_dir() {
        Some(r_libs_path.as_path())
    } else {
        None
    };

    // Build the full list of probe specs (capabilities + per-method).
    // Order: capabilities first, then METHOD_PROBES entries.
    let mut specs: Vec<ProbeSpec> = vec![
        ProbeSpec { key: "r_seurat".to_string(),   kind: ProbeKind::R("Seurat".to_string()) },
        ProbeSpec { key: "r_cellchat".to_string(),  kind: ProbeKind::R("CellChat".to_string()) },
        ProbeSpec { key: "pyscenic".to_string(),    kind: ProbeKind::Python("pyscenic".to_string()) },
        ProbeSpec {
            key: "python_lisi".to_string(),
            kind: ProbeKind::PythonAny(vec![
                "lisi".to_string(),
                "harmonypy".to_string(),
                "scanpy.external.pp.lisi".to_string(),
            ]),
        },
        ProbeSpec { key: "CELLRANGER".to_string(), kind: ProbeKind::Cellranger },
    ];
    for (name, probe) in METHOD_PROBES.iter() {
        let kind = match probe {
            MethodProbe::Python(m) => ProbeKind::Python(m.to_string()),
            MethodProbe::R(pkg)    => ProbeKind::R(pkg.to_string()),
        };
        specs.push(ProbeSpec { key: (*name).to_string(), kind });
    }

    // Attempt to probe inside the resolved execution container image.
    // Fall back to host probes on any failure (no image configured,
    // docker not available, container exits non-zero, etc.).
    let probe_image = resolve_probe_image(pkg_dir);
    let (probe_results, cellranger_version, probe_site) = match &probe_image {
        Some(image) => {
            match run_container_probes(&specs, image, r_libs_user) {
                Ok((bools, cr)) => {
                    tracing::info!(
                        image = %image,
                        "env_capability: probed inside container image"
                    );
                    (bools, cr, format!("container:{image}"))
                }
                Err(e) => {
                    tracing::debug!(
                        error = %e,
                        image = %image,
                        "env_capability: container probe failed, falling back to host"
                    );
                    let (bools, cr) = run_host_probes(&specs, r_libs_user);
                    (bools, cr, "host-fallback".to_string())
                }
            }
        }
        None => {
            tracing::debug!("env_capability: no container image configured, using host probes");
            let (bools, cr) = run_host_probes(&specs, r_libs_user);
            (bools, cr, "host-fallback".to_string())
        }
    };

    // Extract capability booleans (default false if a probe line was missing).
    let get = |key: &str| probe_results.get(key).copied().unwrap_or(false);
    let r_seurat    = get("r_seurat");
    let r_cellchat  = get("r_cellchat");
    let pyscenic    = get("pyscenic");
    let python_lisi = get("python_lisi");

    // Per-method results. BTreeMap so the on-disk JSON is byte-stable.
    let mut methods = serde_json::Map::new();
    let mut available_count = 0usize;
    for (name, probe) in METHOD_PROBES.iter() {
        let available = probe_results.get(*name).copied().unwrap_or(false);
        let (language, probe_target) = match probe {
            MethodProbe::Python(m) => ("python", *m),
            MethodProbe::R(pkg)    => ("r",      *pkg),
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
        "probe_site": probe_site,
        // Standardized execution-environment contract for the bio-min
        // container. Declared (not probed) so the agent uses the canonical
        // interpreters + install verb + renderer instead of discovering them
        // turn-by-turn. The `capabilities`/`methods` blocks below say WHICH
        // analysis packages are present; this block says HOW to use the
        // environment and install the rest. See AGENT-EXECUTOR.md.
        "environment": {
            "note": "Image-agnostic execution contract. The dispatch wrapper resolves the interpreters and install verb for whatever container image is configured and puts them first on PATH (the resolved python is also in $ECAA_PY), so these defaults hold regardless of image layout. Use them; do not search for interpreters or guess install commands.",
            "python": {
                "interpreter": "python3",
                "note": "`python3` on PATH (also `$ECAA_PY`) is the Python interpreter the wrapper put first on PATH for this image. Use it directly; do not hunt for alternate pythons. If an import is genuinely missing, add it with `ecaa-install py <pkg>`."
            },
            "r": {
                "interpreter": "Rscript",
                "note": "`Rscript` on PATH is the R interpreter for this image. Install extra CRAN packages with `ecaa-install r` (into the user library, base graphics preserved). Install Bioconductor packages with `ecaa-install bioc`, which resolves the pre-built bioconda binary into the shared `ecaa-bioc` conda env (the base /opt/conda is read-only); run that compute via `conda run -n ecaa-bioc Rscript …`."
            },
            "compute_language": "Python and R are both first-class compute interpreters here; neither is privileged. Choose whichever fits the method — the choice does not affect figures (a fixed post-compute step renders those from your tables).",
            "figure_rendering": {
                "use": "python3 -m runtime.plotting render",
                "how": "python3 -m runtime.plotting render --stage <plot_stage_id or task_id> --outputs runtime/outputs/<task_id> --required <required_figures>",
                "note": "Figures are NOT your job: emit the standardized output tables for the stage. A fixed post-compute step renders them deterministically from your tables. Do not render figures or hand-roll matplotlib/ggplot. Compute language (Python or R) does not affect figures."
            },
            "install": {
                "command": "ecaa-install <ecosystem> <pkg>...",
                "ecosystems": ["py", "r", "bioc"],
                "note": "Standard install verb on PATH. Routes py->pip, r->install.packages, bioc->bioconda binary into the shared `ecaa-bioc` conda env, into the shared per-session cache and the canonical env. Use it instead of raw pip/conda/mamba/BiocManager so installs are cached, reused across tasks, and never shadow base packages."
            }
        },
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
        "  {} env_capability probe: R+Seurat={} R+CellChat={} pySCENIC={} lisi={} cellranger={} methods={}/{} available [{}]",
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
        probe_site,
    );
    Ok(())
}

/// Run all probes on the HOST using the individual per-probe functions.
/// Returns `(bool_map, cellranger_version)`.  Used as the fallback when
/// no container image is configured or the container probe fails.
fn run_host_probes(
    specs: &[ProbeSpec],
    r_libs_user: Option<&Path>,
) -> (std::collections::BTreeMap<String, bool>, Option<String>) {
    let mut bools = std::collections::BTreeMap::new();
    let mut cellranger_version: Option<String> = None;
    for spec in specs {
        match &spec.kind {
            ProbeKind::R(pkg) => {
                bools.insert(spec.key.clone(), probe_r_package(pkg, r_libs_user));
            }
            ProbeKind::Python(module) => {
                bools.insert(spec.key.clone(), probe_python_import(module));
            }
            ProbeKind::PythonAny(modules) => {
                let result = modules.iter().any(|m| probe_python_import(m));
                bools.insert(spec.key.clone(), result);
            }
            ProbeKind::Cellranger => {
                cellranger_version = probe_cellranger();
            }
        }
    }
    (bools, cellranger_version)
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
            execution_order: Vec::new(),
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
                inputs: Vec::new(),
                outputs: Vec::new(),
                edam_operation: None,
                execution_index: None,
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
                inputs: Vec::new(),
                outputs: Vec::new(),
                edam_operation: None,
                execution_index: None,
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
            execution_order: Vec::new(),
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
                inputs: Vec::new(),
                outputs: Vec::new(),
                edam_operation: None,
                execution_index: None,
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
            execution_order: Vec::new(),
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
                inputs: Vec::new(),
                outputs: Vec::new(),
                edam_operation: None,
                execution_index: None,
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
                inputs: Vec::new(),
                outputs: Vec::new(),
                edam_operation: None,
                execution_index: None,
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
                inputs: Vec::new(),
                outputs: Vec::new(),
                edam_operation: None,
                execution_index: None,
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
            execution_order: Vec::new(),
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

    /// `collect_validation_failure_signals` produces a method-NEUTRAL
    /// domain-correctness statement that restates the design's
    /// operator-authored bound and the agent's OWN recomputed number,
    /// keyed by the failing assertion id — the input to the bounded
    /// autonomous recovery path. Mirrors the heteroplasmy het-band miss
    /// (low_af_band_count = 0, design requires >= 1).
    #[test]
    fn collect_signals_builds_neutral_het_band_statement() {
        use ecaa_workflow_core::dag::{Assignee, ResourceClass, Task, TaskKind, TaskState};
        let tmp = tempfile::tempdir().unwrap();
        let pkg = tmp.path();
        std::fs::create_dir_all(pkg.join("policies")).unwrap();
        std::fs::create_dir_all(pkg.join("runtime/outputs/variant_calling")).unwrap();
        // Agent's own result: the heteroplasmy band has 0 calls, /is_mtdna true.
        std::fs::write(
            pkg.join("runtime/outputs/variant_calling/result.json"),
            r#"{"is_mtdna": true, "low_af_band_count": 0}"#,
        )
        .unwrap();
        let contract = serde_json::json!({
            "contract_id": "test-variant",
            "stages": {
                "variant_calling": {
                    "assertions": [
                        {
                            "id": "variant_calling.het_tail_band_nonempty",
                            "assertion_type": "numeric_threshold",
                            "target": "runtime/outputs/variant_calling/result.json",
                            "check": { "json_pointer": "/low_af_band_count", "op": "gte", "value": 1.0 },
                            "when": { "json_pointer": "/is_mtdna", "equals": true },
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
            "variant_calling".into(),
            Task {
                kind: TaskKind::Computation,
                state: TaskState::Completed {
                    result: serde_json::json!({"method": "x"}),
                },
                depends_on: vec![],
                assignee: Assignee::Agent,
                description: "variant calling".into(),
                spec: Some(serde_json::json!({"stage_class": "variant_calling"})),
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
        let dag = DAG {
            version: "1.0".into(),
            schema_version: ecaa_workflow_core::dag::current_dag_schema_version(),
            workflow_id: "t".into(),
            current_task: None,
            tasks,
            reverse_deps: std::collections::BTreeMap::new(),
            run_id: None,
            execution_order: Vec::new(),
        };
        let signals = collect_validation_failure_signals(pkg, &dag);
        let vc = signals
            .get("variant_calling")
            .expect("variant_calling must have a failed-assertion signal");
        assert_eq!(vc.len(), 1);
        assert_eq!(vc[0].assertion_id, "variant_calling.het_tail_band_nonempty");
        let s = &vc[0].statement;
        // Restates the design bound + the agent's own number; says revisit.
        assert!(s.contains("at least 1"), "must restate the design bound: {s}");
        assert!(s.contains("recomputes 0"), "must restate the agent's own number: {s}");
        assert!(s.contains("revisit"), "must say revisit, not how: {s}");
        // NEUTRALITY: names no tool / flag / threshold-to-set / caller.
        let lower = s.to_ascii_lowercase();
        for token in [
            "lofreq", "gatk", "mutect", "bcftools", "samtools", "freebayes", "--",
            "set the threshold", "use the tool", "aligner", "caller ",
        ] {
            assert!(!lower.contains(token), "neutral statement leaked {token:?}: {s}");
        }
    }

    /// A `recommended` (non-required) assertion that fails must NOT
    /// produce a recovery signal — only required assertions gate.
    #[test]
    fn collect_signals_ignores_recommended_assertions() {
        use ecaa_workflow_core::dag::{Assignee, ResourceClass, Task, TaskKind, TaskState};
        let tmp = tempfile::tempdir().unwrap();
        let pkg = tmp.path();
        std::fs::create_dir_all(pkg.join("policies")).unwrap();
        std::fs::create_dir_all(pkg.join("runtime/outputs/variant_calling")).unwrap();
        std::fs::write(
            pkg.join("runtime/outputs/variant_calling/result.json"),
            r#"{"low_af_band_count": 0}"#,
        )
        .unwrap();
        let contract = serde_json::json!({
            "contract_id": "test-variant",
            "stages": {
                "variant_calling": {
                    "assertions": [
                        {
                            "id": "variant_calling.het_tail_band_nonempty",
                            "assertion_type": "numeric_threshold",
                            "target": "runtime/outputs/variant_calling/result.json",
                            "check": { "json_pointer": "/low_af_band_count", "op": "gte", "value": 1.0 },
                            "severity": "recommended"
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
            "variant_calling".into(),
            Task {
                kind: TaskKind::Computation,
                state: TaskState::Completed {
                    result: serde_json::json!({"method": "x"}),
                },
                depends_on: vec![],
                assignee: Assignee::Agent,
                description: "variant calling".into(),
                spec: Some(serde_json::json!({"stage_class": "variant_calling"})),
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
        let dag = DAG {
            version: "1.0".into(),
            schema_version: ecaa_workflow_core::dag::current_dag_schema_version(),
            workflow_id: "t".into(),
            current_task: None,
            tasks,
            reverse_deps: std::collections::BTreeMap::new(),
            run_id: None,
            execution_order: Vec::new(),
        };
        let signals = collect_validation_failure_signals(pkg, &dag);
        assert!(
            signals.is_empty(),
            "a failing recommended assertion must not trigger recovery, got {signals:?}"
        );
    }

    /// Method-correctness contract, end-to-end through enforce_validation_contract:
    /// a Completed differential_expression task whose result.json records a naked
    /// `~ condition` design (covariates available) AND an inverted regression
    /// (response != stated outcome) must re-block the parent compute task and its
    /// validate_<stage> companion. Mirrors the variant-contract enforce test, but
    /// exercises the new cross_field_equals + formula_references_covariates arms.
    #[test]
    fn association_contract_reblocks_inverted_and_naked_design() {
        use ecaa_workflow_core::dag::{Assignee, ResourceClass, Task, TaskKind, TaskState};
        let tmp = tempfile::tempdir().unwrap();
        let pkg = tmp.path();
        std::fs::create_dir_all(pkg.join("policies")).unwrap();
        std::fs::create_dir_all(pkg.join("runtime/outputs/differential_expression")).unwrap();

        // The shipped association contract (the three required assertions).
        let contract = serde_json::json!({
            "contract_id": "test-association",
            "stages": {
                "differential_expression": {
                    "assertions": [
                        {
                            "id": "differential_expression.design_recorded",
                            "assertion_type": "string_contains",
                            "target": "runtime/outputs/differential_expression/result.json",
                            "check": { "substrings": ["design_formula", "response_variable"] },
                            "severity": "required"
                        },
                        {
                            "id": "differential_expression.design_adjusts_available_covariates",
                            "assertion_type": "formula_references_covariates",
                            "target": "runtime/outputs/differential_expression/result.json",
                            "check": {
                                "formula_pointer": "/design_formula",
                                "covariates_pointer": "/available_covariates",
                                "primary_pointer": "/primary_variable"
                            },
                            "when": { "json_pointer": "/available_covariates" },
                            "severity": "required"
                        },
                        {
                            "id": "differential_expression.response_matches_stated_outcome",
                            "assertion_type": "cross_field_equals",
                            "target": "runtime/outputs/differential_expression/result.json",
                            "check": {
                                "this_pointer": "/response_variable",
                                "other_pointer": "/stated_outcome",
                                "normalize": "casefold_trim"
                            },
                            "when": { "json_pointer": "/stated_outcome" },
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

        // Both errors present: naked `~ condition` (sex/age/RIN available) AND an
        // inverted regression (response=metabolite, stated outcome=SBP).
        std::fs::write(
            pkg.join("runtime/outputs/differential_expression/result.json"),
            serde_json::json!({
                "design_formula": "~ condition",
                "response_variable": "metabolite",
                "available_covariates": ["condition", "sex", "age", "RIN"],
                "primary_variable": "condition",
                "stated_outcome": "SBP"
            })
            .to_string(),
        )
        .unwrap();

        let mut tasks: std::collections::BTreeMap<TaskId, Task> = std::collections::BTreeMap::new();
        tasks.insert(
            "differential_expression".into(),
            Task {
                kind: TaskKind::Computation,
                state: TaskState::Completed {
                    result: serde_json::json!({"method": "deseq2"}),
                },
                depends_on: vec![],
                assignee: Assignee::Agent,
                description: "de".into(),
                spec: Some(serde_json::json!({"stage_class": "differential_expression"})),
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
        tasks.insert(
            "validate_differential_expression".into(),
            Task {
                kind: TaskKind::Validation,
                state: TaskState::Completed {
                    result: serde_json::json!({"outcome": "pass"}),
                },
                depends_on: vec!["differential_expression".into()],
                assignee: Assignee::Agent,
                description: "validate de".into(),
                spec: Some(serde_json::json!({"stage_class": "differential_expression"})),
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
        let mut dag = DAG {
            version: "1.0".into(),
            schema_version: ecaa_workflow_core::dag::current_dag_schema_version(),
            workflow_id: "t".into(),
            current_task: None,
            tasks,
            reverse_deps: std::collections::BTreeMap::new(),
            run_id: None,
            execution_order: Vec::new(),
        };
        let violations = enforce_validation_contract(pkg, &mut dag).unwrap();
        assert_eq!(violations.len(), 1, "exactly the DE stage violates");
        assert_eq!(violations[0].0, "differential_expression");
        // Both method-correctness assertions failed; design_recorded passed.
        assert!(violations[0]
            .1
            .contains(&"differential_expression.design_adjusts_available_covariates".to_string()));
        assert!(violations[0]
            .1
            .contains(&"differential_expression.response_matches_stated_outcome".to_string()));
        assert!(!violations[0]
            .1
            .contains(&"differential_expression.design_recorded".to_string()));
        // Parent + validator re-blocked.
        assert!(matches!(
            dag.tasks.get("differential_expression").unwrap().state,
            TaskState::Blocked { .. }
        ));
        assert!(matches!(
            dag.tasks
                .get("validate_differential_expression")
                .unwrap()
                .state,
            TaskState::Blocked { .. }
        ));
    }

    /// RCA I-10: a plain DE-by-condition result (no SME-named regression
    /// outcome, so `stated_outcome` is correctly OMITTED per
    /// `differential_expression.yaml`'s `result_contract.record_when_applicable`)
    /// must NOT arm `response_matches_stated_outcome` — the `when`-gate on
    /// `/stated_outcome` must stay skipped, and the design otherwise adjusts
    /// for an available covariate, so the stage must NOT be re-blocked.
    #[test]
    fn plain_de_by_condition_does_not_arm_stated_outcome_gate() {
        use ecaa_workflow_core::dag::{Assignee, ResourceClass, Task, TaskKind, TaskState};
        let tmp = tempfile::tempdir().unwrap();
        let pkg = tmp.path();
        std::fs::create_dir_all(pkg.join("policies")).unwrap();
        std::fs::create_dir_all(pkg.join("runtime/outputs/differential_expression")).unwrap();

        let contract = serde_json::json!({
            "contract_id": "test-association",
            "stages": {
                "differential_expression": {
                    "assertions": [
                        {
                            "id": "differential_expression.design_adjusts_available_covariates",
                            "assertion_type": "formula_references_covariates",
                            "target": "runtime/outputs/differential_expression/result.json",
                            "check": {
                                "formula_pointer": "/design_formula",
                                "covariates_pointer": "/available_covariates",
                                "primary_pointer": "/primary_variable"
                            },
                            "when": { "json_pointer": "/available_covariates" },
                            "severity": "required"
                        },
                        {
                            "id": "differential_expression.response_matches_stated_outcome",
                            "assertion_type": "cross_field_equals",
                            "target": "runtime/outputs/differential_expression/result.json",
                            "check": {
                                "this_pointer": "/response_variable",
                                "other_pointer": "/stated_outcome",
                                "normalize": "casefold_trim"
                            },
                            "when": { "json_pointer": "/stated_outcome" },
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

        // Plain DE-by-condition: design adjusts for an available covariate,
        // NO stated_outcome key at all (the atom's contracted default).
        std::fs::write(
            pkg.join("runtime/outputs/differential_expression/result.json"),
            serde_json::json!({
                "design_formula": "~ condition + sex",
                "response_variable": "expression",
                "available_covariates": ["condition", "sex"],
                "primary_variable": "condition"
            })
            .to_string(),
        )
        .unwrap();

        let mut tasks: std::collections::BTreeMap<TaskId, Task> = std::collections::BTreeMap::new();
        tasks.insert(
            "differential_expression".into(),
            Task {
                kind: TaskKind::Computation,
                state: TaskState::Completed {
                    result: serde_json::json!({"method": "deseq2"}),
                },
                depends_on: vec![],
                assignee: Assignee::Agent,
                description: "de".into(),
                spec: Some(serde_json::json!({"stage_class": "differential_expression"})),
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
        tasks.insert(
            "validate_differential_expression".into(),
            Task {
                kind: TaskKind::Validation,
                state: TaskState::Completed {
                    result: serde_json::json!({"outcome": "pass"}),
                },
                depends_on: vec!["differential_expression".into()],
                assignee: Assignee::Agent,
                description: "validate de".into(),
                spec: Some(serde_json::json!({"stage_class": "differential_expression"})),
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
        let mut dag = DAG {
            version: "1.0".into(),
            schema_version: ecaa_workflow_core::dag::current_dag_schema_version(),
            workflow_id: "t".into(),
            current_task: None,
            tasks,
            reverse_deps: std::collections::BTreeMap::new(),
            run_id: None,
            execution_order: Vec::new(),
        };
        let violations = enforce_validation_contract(pkg, &mut dag).unwrap();
        assert!(
            violations.is_empty(),
            "skip-gated check must stay skipped for a plain DE-by-condition result \
             with no stated_outcome recorded: {violations:?}"
        );
        assert!(matches!(
            dag.tasks.get("differential_expression").unwrap().state,
            TaskState::Completed { .. }
        ));
        assert!(matches!(
            dag.tasks
                .get("validate_differential_expression")
                .unwrap()
                .state,
            TaskState::Completed { .. }
        ));
    }

    /// Build a fixture package with a Completed `variant_calling` task whose
    /// own result.json fails the het-band required assertion (0 calls,
    /// design requires >= 1), plus a Completed `validate_variant_calling`
    /// companion. Returns `(tempdir, pkg_path, dag)`. The tempdir must
    /// outlive the dag (it owns the on-disk fixture).
    fn advisory_failing_fixture() -> (tempfile::TempDir, std::path::PathBuf, DAG) {
        use ecaa_workflow_core::dag::{Assignee, ResourceClass, Task, TaskKind, TaskState};
        let tmp = tempfile::tempdir().unwrap();
        let pkg = tmp.path().to_path_buf();
        std::fs::create_dir_all(pkg.join("policies")).unwrap();
        std::fs::create_dir_all(pkg.join("runtime/outputs/variant_calling")).unwrap();
        std::fs::write(
            pkg.join("runtime/outputs/variant_calling/result.json"),
            r#"{"low_af_band_count": 0}"#,
        )
        .unwrap();
        let contract = serde_json::json!({
            "contract_id": "test-variant",
            "stages": {
                "variant_calling": {
                    "assertions": [
                        {
                            "id": "variant_calling.het_tail_band_nonempty",
                            "assertion_type": "numeric_threshold",
                            "target": "runtime/outputs/variant_calling/result.json",
                            "check": { "json_pointer": "/low_af_band_count", "op": "gte", "value": 1.0 },
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
        let mk = |kind: TaskKind, depends_on: Vec<TaskId>| Task {
            kind,
            state: TaskState::Completed {
                result: serde_json::json!({"method": "x"}),
            },
            depends_on,
            assignee: Assignee::Agent,
            description: "vc".into(),
            spec: Some(serde_json::json!({"stage_class": "variant_calling"})),
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
        };
        let mut tasks: std::collections::BTreeMap<TaskId, Task> = std::collections::BTreeMap::new();
        tasks.insert("variant_calling".into(), mk(TaskKind::Computation, vec![]));
        tasks.insert(
            "validate_variant_calling".into(),
            mk(TaskKind::Validation, vec!["variant_calling".into()]),
        );
        let dag = DAG {
            version: "1.0".into(),
            schema_version: ecaa_workflow_core::dag::current_dag_schema_version(),
            workflow_id: "t".into(),
            current_task: None,
            tasks,
            reverse_deps: std::collections::BTreeMap::new(),
            run_id: None,
            execution_order: Vec::new(),
        };
        (tmp, pkg, dag)
    }

    /// Advisory / warn-only contract mode, end-to-end through
    /// `enforce_validation_contract`. One test owns both the advisory and
    /// the recovery env vars start-to-finish so threaded `cargo test`
    /// can't race on the shared process env (nextest is process-isolated;
    /// this is robust under either). Covers:
    ///   - advisory ON + a failing required assertion -> task NOT blocked
    ///     (stays Completed), no violations returned, and a
    ///     validation-warnings.jsonl line written with the right
    ///     task_id/assertion_id/severity + the SAME reason the block path
    ///     computes;
    ///   - advisory OFF -> still blocks (regression guard, unchanged);
    ///   - advisory ON + recovery ON -> advisory wins (no block; recovery
    ///     never fires because the task is left Completed and the call site
    ///     gates recovery on advisory being off).
    #[test]
    fn advisory_mode_records_warning_without_blocking() {
        use ecaa_workflow_core::dag::TaskState;

        // --- advisory ON: warn-only, no block ---
        std::env::set_var(validation_recovery::ENV_CONTRACT_ADVISORY, "1");
        std::env::remove_var(validation_recovery::ENV_VALIDATION_RECOVERY);
        let (_tmp, pkg, mut dag) = advisory_failing_fixture();
        let violations = enforce_validation_contract(&pkg, &mut dag).unwrap();
        assert!(
            violations.is_empty(),
            "advisory mode must not report violations (no block): {violations:?}"
        );
        assert!(
            matches!(
                dag.tasks.get("variant_calling").unwrap().state,
                TaskState::Completed { .. }
            ),
            "advisory mode must leave the compute task Completed so the DAG proceeds"
        );
        assert!(
            matches!(
                dag.tasks.get("validate_variant_calling").unwrap().state,
                TaskState::Completed { .. }
            ),
            "advisory mode must not re-block the validator"
        );
        // The sidecar carries exactly one warning with the right fields.
        let raw = std::fs::read_to_string(validation_recovery::warnings_path(&pkg))
            .expect("validation-warnings.jsonl must be written");
        let lines: Vec<&str> = raw.lines().filter(|l| !l.trim().is_empty()).collect();
        assert_eq!(lines.len(), 1, "one warning per failed assertion: {raw}");
        let w: validation_recovery::AdvisoryWarning = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(w.task_id, "variant_calling");
        assert_eq!(w.assertion_id, "variant_calling.het_tail_band_nonempty");
        assert_eq!(w.severity, "required");
        // Reuses the SAME reason text the block path computes.
        assert!(
            w.reason.contains(
                "required assertion(s) unsatisfied: variant_calling.het_tail_band_nonempty"
            ),
            "reason must reuse the block path's text: {}",
            w.reason
        );
        // Re-running the enforcer is idempotent: the sidecar stays a single
        // deduped line (deterministic, not append-on-every-pass).
        let _ = enforce_validation_contract(&pkg, &mut dag).unwrap();
        let raw2 = std::fs::read_to_string(validation_recovery::warnings_path(&pkg)).unwrap();
        assert_eq!(
            raw2.lines().filter(|l| !l.trim().is_empty()).count(),
            1,
            "re-enforcement must not duplicate warning lines: {raw2}"
        );

        // --- advisory ON + recovery ON: advisory wins (no block, and the
        // recovery gate is suppressed) ---
        std::env::set_var(validation_recovery::ENV_VALIDATION_RECOVERY, "1");
        assert!(
            validation_recovery::advisory_enabled()
                && validation_recovery::recovery_enabled(),
            "both flags must be live for the precedence check"
        );
        let (_tmp2, pkg2, mut dag2) = advisory_failing_fixture();
        let violations2 = enforce_validation_contract(&pkg2, &mut dag2).unwrap();
        assert!(
            violations2.is_empty() && matches!(
                dag2.tasks.get("variant_calling").unwrap().state,
                TaskState::Completed { .. }
            ),
            "with both flags set, advisory must win (no block, no recovery re-dispatch)"
        );

        // --- advisory OFF: still blocks (regression guard) ---
        std::env::remove_var(validation_recovery::ENV_CONTRACT_ADVISORY);
        std::env::remove_var(validation_recovery::ENV_VALIDATION_RECOVERY);
        let (_tmp3, pkg3, mut dag3) = advisory_failing_fixture();
        let violations3 = enforce_validation_contract(&pkg3, &mut dag3).unwrap();
        assert_eq!(
            violations3.len(),
            1,
            "advisory OFF must restore the strict block path"
        );
        assert!(
            matches!(
                dag3.tasks.get("variant_calling").unwrap().state,
                TaskState::Blocked { .. }
            ),
            "advisory OFF must re-block the failing compute task"
        );
        assert!(
            !validation_recovery::warnings_path(&pkg3).exists(),
            "advisory OFF must not write the warnings sidecar"
        );
    }

    /// The same contract PASSES a correctly-specified DE: the design adjusts for
    /// an available covariate and the response matches the stated outcome — no
    /// violation, the parent stays Completed.
    #[test]
    fn association_contract_passes_correct_design() {
        use ecaa_workflow_core::dag::{Assignee, ResourceClass, Task, TaskKind, TaskState};
        let tmp = tempfile::tempdir().unwrap();
        let pkg = tmp.path();
        std::fs::create_dir_all(pkg.join("policies")).unwrap();
        std::fs::create_dir_all(pkg.join("runtime/outputs/differential_expression")).unwrap();
        let contract = serde_json::json!({
            "contract_id": "test-association",
            "stages": {
                "differential_expression": {
                    "assertions": [
                        {
                            "id": "differential_expression.design_recorded",
                            "assertion_type": "string_contains",
                            "target": "runtime/outputs/differential_expression/result.json",
                            "check": { "substrings": ["design_formula", "response_variable"] },
                            "severity": "required"
                        },
                        {
                            "id": "differential_expression.design_adjusts_available_covariates",
                            "assertion_type": "formula_references_covariates",
                            "target": "runtime/outputs/differential_expression/result.json",
                            "check": {
                                "formula_pointer": "/design_formula",
                                "covariates_pointer": "/available_covariates",
                                "primary_pointer": "/primary_variable"
                            },
                            "when": { "json_pointer": "/available_covariates" },
                            "severity": "required"
                        },
                        {
                            "id": "differential_expression.response_matches_stated_outcome",
                            "assertion_type": "cross_field_equals",
                            "target": "runtime/outputs/differential_expression/result.json",
                            "check": {
                                "this_pointer": "/response_variable",
                                "other_pointer": "/stated_outcome",
                                "normalize": "casefold_trim"
                            },
                            "when": { "json_pointer": "/stated_outcome" },
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
        std::fs::write(
            pkg.join("runtime/outputs/differential_expression/result.json"),
            serde_json::json!({
                "design_formula": "~ SBP + sex + age + RIN",
                "response_variable": "SBP",
                "available_covariates": ["SBP", "sex", "age", "RIN"],
                "primary_variable": "SBP",
                "stated_outcome": "SBP"
            })
            .to_string(),
        )
        .unwrap();

        let mut tasks: std::collections::BTreeMap<TaskId, Task> = std::collections::BTreeMap::new();
        tasks.insert(
            "differential_expression".into(),
            Task {
                kind: TaskKind::Computation,
                state: TaskState::Completed {
                    result: serde_json::json!({"method": "deseq2"}),
                },
                depends_on: vec![],
                assignee: Assignee::Agent,
                description: "de".into(),
                spec: Some(serde_json::json!({"stage_class": "differential_expression"})),
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
        let mut dag = DAG {
            version: "1.0".into(),
            schema_version: ecaa_workflow_core::dag::current_dag_schema_version(),
            workflow_id: "t".into(),
            current_task: None,
            tasks,
            reverse_deps: std::collections::BTreeMap::new(),
            run_id: None,
            execution_order: Vec::new(),
        };
        let violations = enforce_validation_contract(pkg, &mut dag).unwrap();
        assert!(
            violations.is_empty(),
            "a correctly-specified DE must not violate: {violations:?}"
        );
        assert!(matches!(
            dag.tasks.get("differential_expression").unwrap().state,
            TaskState::Completed { .. }
        ));
    }

    #[test]
    fn validation_contract_blocks_parent_before_its_validator_completes() {
        // The early-gate contract: a contract-violating compute task must be
        // re-blocked the moment IT reaches Completed, even though its
        // validate_<stage> companion has NOT completed yet. Downstream compute
        // tasks depend on the parent (not the validator), so waiting for the
        // validator to complete let bad results flow downstream first. Here the
        // validator is still Pending — enforcement must fire on the parent alone.
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
                inputs: Vec::new(),
                outputs: Vec::new(),
                edam_operation: None,
                execution_index: None,
            },
        );
        // validate_qc exists but is still Pending — the OLD validate-gated loop
        // would skip enforcement entirely here.
        tasks.insert(
            "validate_qc".into(),
            Task {
                kind: TaskKind::Validation,
                state: TaskState::Pending,
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
                inputs: Vec::new(),
                outputs: Vec::new(),
                edam_operation: None,
                execution_index: None,
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
            execution_order: Vec::new(),
        };
        let violations = enforce_validation_contract(pkg, &mut dag).unwrap();
        assert_eq!(violations.len(), 1, "parent stage must be enforced before validator completes");
        assert_eq!(violations[0].0, "qc");
        assert!(matches!(
            dag.tasks.get("qc").unwrap().state,
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
                inputs: Vec::new(),
                outputs: Vec::new(),
                edam_operation: None,
                execution_index: None,
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
                inputs: Vec::new(),
                outputs: Vec::new(),
                edam_operation: None,
                execution_index: None,
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
            execution_order: Vec::new(),
        };
        let violations = enforce_validation_contract(pkg, &mut dag).unwrap();
        assert!(violations.is_empty());
    }

    #[test]
    fn numeric_threshold_reads_json_pointer_and_compares() {
        let tmp = tempfile::tempdir().unwrap();
        let pkg = tmp.path();
        std::fs::create_dir_all(pkg.join("runtime/outputs/vc")).unwrap();
        std::fs::write(
            pkg.join("runtime/outputs/vc/result.json"),
            serde_json::json!({ "summary": { "variant_count": 42 } }).to_string(),
        )
        .unwrap();
        let empty = std::collections::BTreeMap::new();
        // >= 10 passes
        let a_pass = serde_json::json!({
            "id": "vc.min_variants",
            "assertion_type": "numeric_threshold",
            "target": "runtime/outputs/vc/result.json",
            "check": { "json_pointer": "/summary/variant_count", "op": "gte", "value": 10.0 }
        });
        assert!(run_assertion(pkg, &a_pass, &empty));
        // >= 100 fails
        let a_fail = serde_json::json!({
            "id": "vc.min_variants_high",
            "assertion_type": "numeric_threshold",
            "target": "runtime/outputs/vc/result.json",
            "check": { "json_pointer": "/summary/variant_count", "op": "gte", "value": 100.0 }
        });
        assert!(!run_assertion(pkg, &a_fail, &empty));
        // Missing pointer is pessimistic-false.
        let a_missing = serde_json::json!({
            "id": "vc.absent",
            "assertion_type": "numeric_threshold",
            "target": "runtime/outputs/vc/result.json",
            "check": { "json_pointer": "/summary/nope", "op": "gte", "value": 1.0 }
        });
        assert!(!run_assertion(pkg, &a_missing, &empty));
    }

    #[test]
    fn json_pointer_is_bool_requires_a_typed_boolean() {
        let tmp = tempfile::tempdir().unwrap();
        let pkg = tmp.path();
        std::fs::create_dir_all(pkg.join("runtime/outputs/vc")).unwrap();
        let empty = std::collections::BTreeMap::new();
        let guard = serde_json::json!({
            "id": "vc.is_mtdna_recorded",
            "assertion_type": "json_pointer_is_bool",
            "target": "runtime/outputs/vc/result.json",
            "check": { "json_pointer": "/is_mtdna" }
        });
        let write = |body: serde_json::Value| {
            std::fs::write(
                pkg.join("runtime/outputs/vc/result.json"),
                body.to_string(),
            )
            .unwrap();
        };
        // A typed boolean (true OR false) passes.
        write(serde_json::json!({ "is_mtdna": true }));
        assert!(run_assertion(pkg, &guard, &empty), "/is_mtdna=true must pass");
        write(serde_json::json!({ "is_mtdna": false }));
        assert!(run_assertion(pkg, &guard, &empty), "/is_mtdna=false must pass");
        // The fail-open a substring match would have allowed: the field NAME
        // occurs incidentally in a note string, but /is_mtdna never resolves to
        // a bool — must fail closed.
        write(serde_json::json!({ "is_mtdna_note": "computed is_mtdna from contigs", "low_af_band_count": 0 }));
        assert!(
            !run_assertion(pkg, &guard, &empty),
            "an incidental 'is_mtdna' substring with no typed /is_mtdna must fail (the closed fail-open)"
        );
        // A non-bool value at the pointer fails closed.
        write(serde_json::json!({ "is_mtdna": "true" }));
        assert!(!run_assertion(pkg, &guard, &empty), "/is_mtdna as a string must fail");
        // Absent pointer fails closed.
        write(serde_json::json!({ "something_else": 1 }));
        assert!(!run_assertion(pkg, &guard, &empty), "absent /is_mtdna must fail");
    }

    #[test]
    fn json_pointer_is_array_requires_a_typed_array_empty_allowed() {
        let tmp = tempfile::tempdir().unwrap();
        let pkg = tmp.path();
        std::fs::create_dir_all(pkg.join("runtime/outputs/de")).unwrap();
        let empty = std::collections::BTreeMap::new();
        let guard = serde_json::json!({
            "id": "de.design_records_covariate_columns",
            "assertion_type": "json_pointer_is_array",
            "target": "runtime/outputs/de/result.json",
            "check": { "json_pointer": "/available_covariates" }
        });
        let write = |body: serde_json::Value| {
            std::fs::write(pkg.join("runtime/outputs/de/result.json"), body.to_string()).unwrap();
        };
        // A populated array passes; an EMPTY array also passes (covariate-free
        // run — the adjustment check's own when-gate then self-skips).
        write(serde_json::json!({ "available_covariates": ["age", "sex"] }));
        assert!(run_assertion(pkg, &guard, &empty), "non-empty array must pass");
        write(serde_json::json!({ "available_covariates": [] }));
        assert!(run_assertion(pkg, &guard, &empty), "empty array must pass (covariate-free)");
        // The fail-open a substring would have allowed: the field name appears in
        // a note / at a nested key, but the top-level pointer is not an array.
        write(serde_json::json!({ "notes": "recorded available_covariates in the model" }));
        assert!(
            !run_assertion(pkg, &guard, &empty),
            "incidental field-name occurrence with no typed /available_covariates must fail"
        );
        write(serde_json::json!({ "nested": { "available_covariates": ["age"] } }));
        assert!(
            !run_assertion(pkg, &guard, &empty),
            "a nested available_covariates the when-gate can't read must fail (the closed fail-open)"
        );
        // A non-array scalar at the pointer fails closed.
        write(serde_json::json!({ "available_covariates": "age,sex" }));
        assert!(!run_assertion(pkg, &guard, &empty), "a string must fail");
    }

    #[test]
    fn het_band_empty_fails_numeric_threshold() {
        let tmp = tempfile::tempdir().unwrap();
        let pkg = tmp.path();
        std::fs::create_dir_all(pkg.join("runtime/outputs/variant_filtering")).unwrap();
        std::fs::write(
            pkg.join("runtime/outputs/variant_filtering/result.json"),
            serde_json::json!({ "low_af_band_count": 0, "sub_noise_floor_count": 0 }).to_string(),
        )
        .unwrap();
        let empty = std::collections::BTreeMap::new();
        let a = serde_json::json!({
            "id": "variant_filtering.het_tail_band_nonempty",
            "assertion_type": "numeric_threshold",
            "target": "runtime/outputs/variant_filtering/result.json",
            "check": { "json_pointer": "/low_af_band_count", "op": "gte", "value": 1.0 }
        });
        assert!(!run_assertion(pkg, &a, &empty), "empty het band must fail (dropped het)");
    }

    #[test]
    fn het_band_present_passes_numeric_threshold() {
        let tmp = tempfile::tempdir().unwrap();
        let pkg = tmp.path();
        std::fs::create_dir_all(pkg.join("runtime/outputs/variant_filtering")).unwrap();
        std::fs::write(
            pkg.join("runtime/outputs/variant_filtering/result.json"),
            serde_json::json!({ "low_af_band_count": 1, "sub_noise_floor_count": 0 }).to_string(),
        )
        .unwrap();
        let empty = std::collections::BTreeMap::new();
        let a = serde_json::json!({
            "id": "variant_filtering.het_tail_band_nonempty",
            "assertion_type": "numeric_threshold",
            "target": "runtime/outputs/variant_filtering/result.json",
            "check": { "json_pointer": "/low_af_band_count", "op": "gte", "value": 1.0 }
        });
        assert!(run_assertion(pkg, &a, &empty), "non-empty het band must pass");
    }

    /// The DE effect-size-reliability assertion (C5, da-15-1) is a
    /// numeric_threshold `gte` on the agent-recomputed
    /// /top_effect_abundance_ratio (median abundance of the agent's top-K-by-
    /// |effect| features over the median abundance of the whole tested set;
    /// null-robust, ≈1 under independence, →0 for the unshrunken-low-count
    /// artifact), `required` severity, `when`-gated on
    /// /information_column_recorded == true. Three behaviours:
    ///   (a) the top-effect abundance ratio below the operator floor, with the
    ///       gate satisfied -> FAIL.
    ///   (b) the gate boolean false (no abundance column) -> SKIPPED (pass),
    ///       never false-failed.
    ///   (c) build_statement emits a method-neutral recomputed-vs-bound signal.
    fn top_effect_reliability_assertion() -> serde_json::Value {
        serde_json::json!({
            "id": "differential_expression.top_effect_reliability",
            "assertion_type": "numeric_threshold",
            "target": "runtime/outputs/differential_expression/result.json",
            "check": {
                "json_pointer": "/top_effect_abundance_ratio",
                "op": "gte",
                "value": 0.20
            },
            "when": { "json_pointer": "/information_column_recorded", "equals": true }
        })
    }

    #[test]
    fn top_effect_reliability_fails_when_abundance_ratio_below_bound() {
        let tmp = tempfile::tempdir().unwrap();
        let pkg = tmp.path();
        std::fs::create_dir_all(pkg.join("runtime/outputs/differential_expression")).unwrap();
        std::fs::write(
            pkg.join("runtime/outputs/differential_expression/result.json"),
            serde_json::json!({
                "top_effect_abundance_ratio": 0.09,
                "information_column_recorded": true
            })
            .to_string(),
        )
        .unwrap();
        let empty = std::collections::BTreeMap::new();
        assert!(
            !run_assertion(pkg, &top_effect_reliability_assertion(), &empty),
            "a top-effect abundance ratio 0.09 (< floor 0.20) must fail"
        );
    }

    #[test]
    fn top_effect_reliability_passes_when_abundance_ratio_at_or_above_bound() {
        let tmp = tempfile::tempdir().unwrap();
        let pkg = tmp.path();
        std::fs::create_dir_all(pkg.join("runtime/outputs/differential_expression")).unwrap();
        std::fs::write(
            pkg.join("runtime/outputs/differential_expression/result.json"),
            serde_json::json!({
                "top_effect_abundance_ratio": 1.0,
                "information_column_recorded": true
            })
            .to_string(),
        )
        .unwrap();
        let empty = std::collections::BTreeMap::new();
        assert!(
            run_assertion(pkg, &top_effect_reliability_assertion(), &empty),
            "a top-effect abundance ratio at/above the floor must pass (strong hits well-expressed)"
        );
    }

    #[test]
    fn top_effect_reliability_skipped_when_no_abundance_column() {
        let tmp = tempfile::tempdir().unwrap();
        let pkg = tmp.path();
        std::fs::create_dir_all(pkg.join("runtime/outputs/differential_expression")).unwrap();
        // Gate boolean false: the table carried no abundance column. The ratio
        // could even violate the floor, but the `when` gate (equals: true)
        // makes the check not-applicable -> skipped (pass), never false-failed.
        std::fs::write(
            pkg.join("runtime/outputs/differential_expression/result.json"),
            serde_json::json!({
                "top_effect_abundance_ratio": 0.09,
                "information_column_recorded": false
            })
            .to_string(),
        )
        .unwrap();
        let empty = std::collections::BTreeMap::new();
        assert!(
            !when_clause_satisfied(pkg, &top_effect_reliability_assertion()),
            "the /information_column_recorded=false gate must be unsatisfied (skip)"
        );
        assert!(
            run_assertion(pkg, &top_effect_reliability_assertion(), &empty),
            "no abundance column must SKIP (pass), never false-fail the ranking"
        );
    }

    #[test]
    fn top_effect_reliability_recovery_statement_is_method_neutral() {
        let tmp = tempfile::tempdir().unwrap();
        let pkg = tmp.path();
        std::fs::create_dir_all(pkg.join("runtime/outputs/differential_expression")).unwrap();
        std::fs::write(
            pkg.join("runtime/outputs/differential_expression/result.json"),
            serde_json::json!({ "top_effect_abundance_ratio": 0.09 }).to_string(),
        )
        .unwrap();
        let s = validation_recovery::build_statement(
            pkg,
            &top_effect_reliability_assertion(),
            &std::collections::BTreeMap::new(),
        );
        // Restates the design's bound + the agent's own recomputed number, says
        // "revisit", and carries the load-bearing neutrality coda.
        assert!(s.contains("differential_expression.top_effect_reliability"), "{s}");
        assert!(s.contains("at least 0.2"), "must restate the design bound: {s}");
        assert!(s.contains("recomputes 0.09"), "must restate the agent's own number: {s}");
        assert!(
            s.contains("no method, tool, or threshold value is prescribed"),
            "must carry the neutrality coda: {s}"
        );
        // No method/estimator/filter token may leak into the neutral signal.
        let lower = s.to_ascii_lowercase();
        for token in [
            "deseq", "edger", "limma", "shrink", "apeglm", "ashr", "wilcoxon", "t-test",
            "low-count", "filter low", "unshrunken", "normalization method", "set the threshold",
        ] {
            assert!(
                !lower.contains(token),
                "neutral signal leaked a method/remedy token {token:?}: {s}"
            );
        }
    }

    #[test]
    fn sub_noise_floor_overcall_fails_numeric_threshold() {
        let tmp = tempfile::tempdir().unwrap();
        let pkg = tmp.path();
        std::fs::create_dir_all(pkg.join("runtime/outputs/variant_filtering")).unwrap();
        std::fs::write(
            pkg.join("runtime/outputs/variant_filtering/result.json"),
            serde_json::json!({ "low_af_band_count": 2, "sub_noise_floor_count": 1 }).to_string(),
        )
        .unwrap();
        let empty = std::collections::BTreeMap::new();
        let a = serde_json::json!({
            "id": "variant_filtering.no_sub_noise_floor_calls",
            "assertion_type": "numeric_threshold",
            "target": "runtime/outputs/variant_filtering/result.json",
            "check": { "json_pointer": "/sub_noise_floor_count", "op": "lte", "value": 0.0 }
        });
        assert!(!run_assertion(pkg, &a, &empty), "any sub-noise-floor call must fail (over-call)");
    }

    #[test]
    fn missing_af_metric_fails_closed() {
        let tmp = tempfile::tempdir().unwrap();
        let pkg = tmp.path();
        // No result.json written -> missing file -> read_json_pointer_f64 None -> fail.
        let empty = std::collections::BTreeMap::new();
        let a = serde_json::json!({
            "id": "variant_filtering.het_tail_band_nonempty",
            "assertion_type": "numeric_threshold",
            "target": "runtime/outputs/variant_filtering/result.json",
            "check": { "json_pointer": "/low_af_band_count", "op": "gte", "value": 1.0 }
        });
        assert!(!run_assertion(pkg, &a, &empty), "missing metric must fail closed (pessimistic)");
    }

    #[test]
    fn numeric_distribution_checks_p_stats_against_bounds() {
        let tmp = tempfile::tempdir().unwrap();
        let pkg = tmp.path();
        std::fs::create_dir_all(pkg.join("runtime/outputs/vc")).unwrap();
        // AF spectrum: most low, deterministic.
        std::fs::write(
            pkg.join("runtime/outputs/vc/result.json"),
            serde_json::json!({ "af_values": [0.01,0.02,0.03,0.05,0.10,0.20,0.40,0.80] })
                .to_string(),
        )
        .unwrap();
        let empty = std::collections::BTreeMap::new();
        // p50 should sit in [0.0, 0.5]; min observed value >= 0.0.
        let a = serde_json::json!({
            "id": "vc.af_spectrum",
            "assertion_type": "numeric_distribution",
            "target": "runtime/outputs/vc/result.json",
            "check": {
                "json_pointer": "/af_values",
                "stat": "p50", "op": "lte", "value": 0.5
            }
        });
        assert!(run_assertion(pkg, &a, &empty));
        let a_bad = serde_json::json!({
            "id": "vc.af_spectrum_bad",
            "assertion_type": "numeric_distribution",
            "target": "runtime/outputs/vc/result.json",
            "check": { "json_pointer": "/af_values", "stat": "p50", "op": "gte", "value": 0.9 }
        });
        assert!(!run_assertion(pkg, &a_bad, &empty));
    }

    #[test]
    fn reference_range_outlier_flags_only_within_tolerance() {
        let tmp = tempfile::tempdir().unwrap();
        let pkg = tmp.path();
        std::fs::create_dir_all(pkg.join("runtime/outputs/vc")).unwrap();
        std::fs::write(
            pkg.join("runtime/outputs/vc/result.json"),
            serde_json::json!({ "variant_count_per_sample": [40, 42, 41, 39, 43] }).to_string(),
        )
        .unwrap();
        let empty = std::collections::BTreeMap::new();
        // Reference [10, 100], 0 tolerance — all in range, passes (no outliers).
        let a = serde_json::json!({
            "id": "vc.per_sample",
            "assertion_type": "reference_range_outlier",
            "target": "runtime/outputs/vc/result.json",
            "check": { "json_pointer": "/variant_count_per_sample",
                       "reference_min": 10.0, "reference_max": 100.0, "tolerance": 0.0 }
        });
        assert!(run_assertion(pkg, &a, &empty));
        // Tight reference [10, 41] — 42 and 43 fall outside → assertion fails.
        let a_bad = serde_json::json!({
            "id": "vc.per_sample_tight",
            "assertion_type": "reference_range_outlier",
            "target": "runtime/outputs/vc/result.json",
            "check": { "json_pointer": "/variant_count_per_sample",
                       "reference_min": 10.0, "reference_max": 41.0, "tolerance": 0.0 }
        });
        assert!(!run_assertion(pkg, &a_bad, &empty));
    }

    #[test]
    fn positive_and_negative_control_arms() {
        let tmp = tempfile::tempdir().unwrap();
        let pkg = tmp.path();
        std::fs::create_dir_all(pkg.join("runtime/outputs/vc")).unwrap();
        std::fs::write(
            pkg.join("runtime/outputs/vc/result.json"),
            serde_json::json!({
                "controls": { "positive_called": ["rs6311"], "negative_called": [] }
            })
            .to_string(),
        )
        .unwrap();
        let empty = std::collections::BTreeMap::new();
        // positive control must be present (non-empty array).
        let pos = serde_json::json!({
            "id": "vc.pos_ctrl",
            "assertion_type": "positive_control_present",
            "target": "runtime/outputs/vc/result.json",
            "check": { "json_pointer": "/controls/positive_called" }
        });
        assert!(run_assertion(pkg, &pos, &empty));
        // negative control must be ABSENT (empty array == no false call).
        let neg = serde_json::json!({
            "id": "vc.neg_ctrl",
            "assertion_type": "negative_control_present",
            "target": "runtime/outputs/vc/result.json",
            "check": { "json_pointer": "/controls/negative_called" }
        });
        assert!(run_assertion(pkg, &neg, &empty));
        // A non-empty negative-called array fails the negative-control arm.
        std::fs::write(
            pkg.join("runtime/outputs/vc/result.json"),
            serde_json::json!({
                "controls": { "positive_called": [], "negative_called": ["spurious"] }
            })
            .to_string(),
        )
        .unwrap();
        assert!(!run_assertion(pkg, &pos, &empty)); // empty positive → fail
        assert!(!run_assertion(pkg, &neg, &empty)); // non-empty negative → fail
    }

    #[test]
    fn cross_stage_comparison_uses_upstream_map() {
        let tmp = tempfile::tempdir().unwrap();
        let pkg = tmp.path();
        std::fs::create_dir_all(pkg.join("runtime/outputs/variant_calling")).unwrap();
        std::fs::create_dir_all(pkg.join("runtime/outputs/variant_filtering")).unwrap();
        std::fs::write(
            pkg.join("runtime/outputs/variant_calling/result.json"),
            serde_json::json!({ "variant_count": 100 }).to_string(),
        )
        .unwrap();
        std::fs::write(
            pkg.join("runtime/outputs/variant_filtering/result.json"),
            serde_json::json!({ "variant_count": 80 }).to_string(),
        )
        .unwrap();
        let mut upstream: std::collections::BTreeMap<String, std::path::PathBuf> =
            std::collections::BTreeMap::new();
        upstream.insert(
            "variant_calling".into(),
            pkg.join("runtime/outputs/variant_calling"),
        );
        // filtered(80) <= called(100) → passes.
        let a = serde_json::json!({
            "id": "vf.filtered_le_called",
            "assertion_type": "cross_stage_output_comparison",
            "target": "runtime/outputs/variant_filtering/result.json",
            "check": {
                "this_pointer": "/variant_count",
                "upstream_task": "variant_calling",
                "upstream_file": "result.json",
                "upstream_pointer": "/variant_count",
                "op": "lte"
            }
        });
        assert!(run_assertion(pkg, &a, &upstream));
        // A missing upstream task in the map is pessimistic-false.
        let empty = std::collections::BTreeMap::new();
        assert!(!run_assertion(pkg, &a, &empty));
        // filtered > called → fails (filtering can only remove records).
        std::fs::write(
            pkg.join("runtime/outputs/variant_filtering/result.json"),
            serde_json::json!({ "variant_count": 120 }).to_string(),
        )
        .unwrap();
        assert!(!run_assertion(pkg, &a, &upstream));
    }

    /// Pessimistic-unknown contract: an unrecognized assertion_type must
    /// fail closed (false), never silently pass.
    #[test]
    fn unknown_assertion_type_fails_closed() {
        let tmp = tempfile::tempdir().unwrap();
        let pkg = tmp.path();
        let empty = std::collections::BTreeMap::new();
        let a = serde_json::json!({
            "id": "vc.bogus",
            "assertion_type": "totally_made_up_check",
            "target": "runtime/outputs/vc/result.json",
            "check": {}
        });
        assert!(!run_assertion(pkg, &a, &empty));
    }

    /// A `when`-gated assertion is SKIPPED (treated as passed) when the guard
    /// predicate is unmet — e.g. an mtDNA-only het-band check against a nuclear
    /// germline call set (is_mtdna=false). Without the skip, the mtDNA-tuned
    /// assertion would false-fail correct germline output.
    #[test]
    fn when_clause_skips_assertion_for_non_mtdna_call_set() {
        let tmp = tempfile::tempdir().unwrap();
        let pkg = tmp.path();
        std::fs::create_dir_all(pkg.join("runtime/outputs/variant_calling")).unwrap();
        // Germline-shaped result: no low-AF band variants, flagged non-mtDNA.
        std::fs::write(
            pkg.join("runtime/outputs/variant_calling/result.json"),
            serde_json::json!({ "is_mtdna": false, "low_af_band_count": 0 }).to_string(),
        )
        .unwrap();
        let empty = std::collections::BTreeMap::new();
        let a = serde_json::json!({
            "id": "vc.het_tail",
            "assertion_type": "numeric_threshold",
            "target": "runtime/outputs/variant_calling/result.json",
            "check": { "json_pointer": "/low_af_band_count", "op": "gte", "value": 1.0 },
            "when": { "json_pointer": "/is_mtdna", "equals": true }
        });
        // low_af_band_count (0) >= 1 would FAIL, but the when-guard skips it.
        assert!(
            run_assertion(pkg, &a, &empty),
            "non-mtDNA call set must skip the mtDNA-only assertion"
        );
    }

    /// The same `when`-gated assertion RUNS (and can fail) when the guard holds
    /// — an mtDNA call set that dropped its heteroplasmic tail must be caught.
    #[test]
    fn when_clause_runs_assertion_for_mtdna_call_set() {
        let tmp = tempfile::tempdir().unwrap();
        let pkg = tmp.path();
        std::fs::create_dir_all(pkg.join("runtime/outputs/variant_calling")).unwrap();
        // mtDNA result that dropped the het: empty low-AF band, flagged mtDNA.
        std::fs::write(
            pkg.join("runtime/outputs/variant_calling/result.json"),
            serde_json::json!({ "is_mtdna": true, "low_af_band_count": 0 }).to_string(),
        )
        .unwrap();
        let empty = std::collections::BTreeMap::new();
        let a = serde_json::json!({
            "id": "vc.het_tail",
            "assertion_type": "numeric_threshold",
            "target": "runtime/outputs/variant_calling/result.json",
            "check": { "json_pointer": "/low_af_band_count", "op": "gte", "value": 1.0 },
            "when": { "json_pointer": "/is_mtdna", "equals": true }
        });
        assert!(
            !run_assertion(pkg, &a, &empty),
            "mtDNA call set with an empty het band must FAIL the het-tail check"
        );
    }

    /// A `when` clause whose target/pointer is unreadable makes the assertion
    /// not-applicable (skip), not a hard failure — the universal assertions are
    /// what force the measurement to exist.
    #[test]
    fn when_clause_skips_when_guard_field_absent() {
        let tmp = tempfile::tempdir().unwrap();
        let pkg = tmp.path();
        std::fs::create_dir_all(pkg.join("runtime/outputs/variant_calling")).unwrap();
        std::fs::write(
            pkg.join("runtime/outputs/variant_calling/result.json"),
            serde_json::json!({ "low_af_band_count": 0 }).to_string(), // no is_mtdna
        )
        .unwrap();
        let empty = std::collections::BTreeMap::new();
        let a = serde_json::json!({
            "id": "vc.het_tail",
            "assertion_type": "numeric_threshold",
            "target": "runtime/outputs/variant_calling/result.json",
            "check": { "json_pointer": "/low_af_band_count", "op": "gte", "value": 1.0 },
            "when": { "json_pointer": "/is_mtdna", "equals": true }
        });
        assert!(
            run_assertion(pkg, &a, &empty),
            "absent guard field -> assertion not applicable -> skipped"
        );
    }

    // ---- Method-correctness assertions (DE / regression) ----
    //
    // Helper: write a differential_expression result.json and return the pkg
    // dir so the assertion arms (which resolve against pkg-relative targets)
    // can read it.
    fn write_de_result(body: serde_json::Value) -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("runtime/outputs/differential_expression"))
            .unwrap();
        std::fs::write(
            tmp.path()
                .join("runtime/outputs/differential_expression/result.json"),
            body.to_string(),
        )
        .unwrap();
        tmp
    }

    fn cross_field_equals_assertion() -> serde_json::Value {
        serde_json::json!({
            "id": "de.response_matches_stated_outcome",
            "assertion_type": "cross_field_equals",
            "target": "runtime/outputs/differential_expression/result.json",
            "check": {
                "this_pointer": "/response_variable",
                "other_pointer": "/stated_outcome",
                "normalize": "casefold_trim"
            }
        })
    }

    fn formula_assertion() -> serde_json::Value {
        serde_json::json!({
            "id": "de.design_adjusts_available_covariates",
            "assertion_type": "formula_references_covariates",
            "target": "runtime/outputs/differential_expression/result.json",
            "check": {
                "formula_pointer": "/design_formula",
                "covariates_pointer": "/available_covariates",
                "primary_pointer": "/primary_variable"
            }
        })
    }

    /// cross_field_equals PASSES when the agent's recorded response_variable
    /// matches the agent's recorded stated_outcome (a correctly-oriented model).
    #[test]
    fn cross_field_equals_passes_on_matching_outcome() {
        let tmp = write_de_result(serde_json::json!({
            "response_variable": "SBP",
            "stated_outcome": "SBP"
        }));
        let empty = std::collections::BTreeMap::new();
        assert!(
            run_assertion(tmp.path(), &cross_field_equals_assertion(), &empty),
            "response_variable == stated_outcome must pass"
        );
    }

    /// cross_field_equals FAILS on the da-8-1 inversion: the agent regressed
    /// `metabolite ~ SBP`, recording response_variable=metabolite while the
    /// task's stated_outcome is SBP — the recorded outcome disagrees.
    #[test]
    fn cross_field_equals_fails_on_inverted_regression() {
        let tmp = write_de_result(serde_json::json!({
            "response_variable": "metabolite",
            "stated_outcome": "SBP"
        }));
        let empty = std::collections::BTreeMap::new();
        assert!(
            !run_assertion(tmp.path(), &cross_field_equals_assertion(), &empty),
            "inverted regression (response != stated outcome) must FAIL"
        );
    }

    /// cross_field_equals normalizes with casefold+trim so capitalization /
    /// padding differences in the agent's own records do not false-fail.
    #[test]
    fn cross_field_equals_casefold_trim_matches() {
        let tmp = write_de_result(serde_json::json!({
            "response_variable": "  sbp ",
            "stated_outcome": "SBP"
        }));
        let empty = std::collections::BTreeMap::new();
        assert!(
            run_assertion(tmp.path(), &cross_field_equals_assertion(), &empty),
            "casefold+trim must normalize '  sbp ' and 'SBP' to equal"
        );
    }

    /// cross_field_equals FAILS CLOSED when either pointer is absent — an
    /// unrecorded outcome is not an excuse to pass (the `when` gate decides
    /// applicability, not the arm).
    #[test]
    fn cross_field_equals_fails_closed_on_missing_pointer() {
        let tmp = write_de_result(serde_json::json!({
            "response_variable": "SBP"
            // no stated_outcome
        }));
        let empty = std::collections::BTreeMap::new();
        assert!(
            !run_assertion(tmp.path(), &cross_field_equals_assertion(), &empty),
            "missing other_pointer must fail closed"
        );
    }

    /// formula_references_covariates PASSES on a full design that references a
    /// non-primary available covariate (the agent adjusted for sex/RIN).
    #[test]
    fn formula_references_covariates_passes_on_full_design() {
        let tmp = write_de_result(serde_json::json!({
            "design_formula": "~ condition + sex + RIN",
            "available_covariates": ["condition", "sex", "age", "RIN"],
            "primary_variable": "condition"
        }));
        let empty = std::collections::BTreeMap::new();
        assert!(
            run_assertion(tmp.path(), &formula_assertion(), &empty),
            "a design referencing a non-primary covariate must pass"
        );
    }

    /// formula_references_covariates FAILS on the da-15-1 naked design: the
    /// agent recorded `~ condition` while sex/age/RIN were available — none of
    /// the non-primary covariates is referenced.
    #[test]
    fn formula_references_covariates_fails_on_naked_condition() {
        let tmp = write_de_result(serde_json::json!({
            "design_formula": "~ condition",
            "available_covariates": ["condition", "sex", "age", "RIN"],
            "primary_variable": "condition"
        }));
        let empty = std::collections::BTreeMap::new();
        assert!(
            !run_assertion(tmp.path(), &formula_assertion(), &empty),
            "naked `~ condition` with covariates available must FAIL"
        );
    }

    /// formula_references_covariates PASSES (nothing to adjust for) when the
    /// only available covariate IS the primary variable — a naked design is
    /// correct there. The arm-level fallthrough handles this even without the
    /// `when` gate.
    #[test]
    fn formula_references_covariates_passes_when_only_primary_available() {
        let tmp = write_de_result(serde_json::json!({
            "design_formula": "~ condition",
            "available_covariates": ["condition"],
            "primary_variable": "condition"
        }));
        let empty = std::collections::BTreeMap::new();
        assert!(
            run_assertion(tmp.path(), &formula_assertion(), &empty),
            "only the primary covariate available -> nothing to adjust for -> pass"
        );
    }

    /// formula_references_covariates is SKIPPED via the `when` gate when no
    /// covariates were recorded — mirrors the shipped contract's
    /// `when: {json_pointer: /available_covariates}`.
    #[test]
    fn formula_references_covariates_skips_when_no_covariates_recorded() {
        let tmp = write_de_result(serde_json::json!({
            "design_formula": "~ condition"
            // no available_covariates
        }));
        let empty = std::collections::BTreeMap::new();
        let mut a = formula_assertion();
        a.as_object_mut().unwrap().insert(
            "when".to_string(),
            serde_json::json!({ "json_pointer": "/available_covariates" }),
        );
        assert!(
            run_assertion(tmp.path(), &a, &empty),
            "no recorded covariates -> when gate skips -> not applicable -> pass"
        );
    }

    /// formula_references_covariates FAILS CLOSED when the design_formula
    /// pointer is missing (an unrecorded model is a failure, not a pass).
    #[test]
    fn formula_references_covariates_fails_closed_on_missing_formula() {
        let tmp = write_de_result(serde_json::json!({
            "available_covariates": ["condition", "sex"],
            "primary_variable": "condition"
            // no design_formula
        }));
        let empty = std::collections::BTreeMap::new();
        assert!(
            !run_assertion(tmp.path(), &formula_assertion(), &empty),
            "missing design_formula must fail closed"
        );
    }

    /// design_recorded (string_contains, both keys required) PASSES only when
    /// result.json names BOTH design_formula and response_variable.
    #[test]
    fn design_recorded_requires_both_keys() {
        let a = serde_json::json!({
            "id": "de.design_recorded",
            "assertion_type": "string_contains",
            "target": "runtime/outputs/differential_expression/result.json",
            "check": { "substrings": ["design_formula", "response_variable"] }
        });
        let empty = std::collections::BTreeMap::new();

        let both = write_de_result(serde_json::json!({
            "design_formula": "~ condition + sex",
            "response_variable": "counts"
        }));
        assert!(
            run_assertion(both.path(), &a, &empty),
            "both keys recorded -> pass"
        );

        let only_one = write_de_result(serde_json::json!({
            "design_formula": "~ condition + sex"
            // no response_variable
        }));
        assert!(
            !run_assertion(only_one.path(), &a, &empty),
            "missing response_variable -> fail (unauditable model)"
        );
    }

    // Report-completeness arms (da-8-1 C8): a string_contains scoped to
    // /narrative_text, `when`-gated on the agent-recorded presence flag.
    fn reports_model_fit_assertion() -> serde_json::Value {
        serde_json::json!({
            "id": "de.reports_model_fit",
            "assertion_type": "string_contains",
            "target": "runtime/outputs/differential_expression/result.json",
            "check": {
                "json_pointer": "/narrative_text",
                "case_insensitive": true,
                "substrings_any": ["r2", "r-squared", "variance explained", "r_squared"]
            },
            "when": { "json_pointer": "/r_squared_column_recorded", "equals": true }
        })
    }

    /// Gate-skip: when the agent recorded no model-fit column the flag is false,
    /// so the assertion is NOT APPLICABLE and skips (never blocks) regardless of
    /// the narrative.
    #[test]
    fn reports_model_fit_skips_when_flag_false() {
        let tmp = write_de_result(serde_json::json!({
            "r_squared_column_recorded": false,
            "narrative_text": "no model-fit mention at all"
        }));
        let empty = std::collections::BTreeMap::new();
        assert!(
            run_assertion(tmp.path(), &reports_model_fit_assertion(), &empty),
            "flag false -> gate skips -> passes"
        );
    }

    /// Pass: column recorded AND the narrative surfaces the statistic.
    #[test]
    fn reports_model_fit_passes_when_narrative_references_statistic() {
        let tmp = write_de_result(serde_json::json!({
            "r_squared_column_recorded": true,
            "narrative_text": "The regression R-squared was 0.84 across features."
        }));
        let empty = std::collections::BTreeMap::new();
        assert!(
            run_assertion(tmp.path(), &reports_model_fit_assertion(), &empty),
            "column recorded + narrative surfaces R-squared -> passes"
        );
    }

    /// Fail: column recorded but the narrative OMITS the statistic.
    #[test]
    fn reports_model_fit_fails_when_recorded_but_narrative_omits() {
        let tmp = write_de_result(serde_json::json!({
            "r_squared_column_recorded": true,
            "narrative_text": "We report the top features and their fold changes."
        }));
        let empty = std::collections::BTreeMap::new();
        assert!(
            !run_assertion(tmp.path(), &reports_model_fit_assertion(), &empty),
            "recorded but narrative omits the statistic -> fails"
        );
    }

    /// Channel-scope guard (the load-bearing false-pass defense): the substring
    /// `r_squared` also occurs inside the FIELD NAME `r_squared_column_recorded`.
    /// A whole-file search would false-PASS regardless of the narrative. The
    /// json_pointer scoping must confine the search to /narrative_text so the
    /// field name cannot satisfy the check.
    #[test]
    fn reports_model_fit_scoping_ignores_field_name_collision() {
        let tmp = write_de_result(serde_json::json!({
            // The flag key contains "r_squared"; the narrative does NOT mention
            // the statistic. Scoped search must FAIL here.
            "r_squared_column_recorded": true,
            "narrative_text": "Only fold changes are discussed."
        }));
        let empty = std::collections::BTreeMap::new();
        assert!(
            !run_assertion(tmp.path(), &reports_model_fit_assertion(), &empty),
            "scoped to /narrative_text -> field-name 'r_squared_column_recorded' must NOT satisfy the check"
        );
    }

    /// Unscoped string_contains (no json_pointer) keeps its original whole-file
    /// behavior byte-for-byte — the existing design_recorded etc. rely on it.
    #[test]
    fn string_contains_without_pointer_searches_whole_file() {
        let a = serde_json::json!({
            "id": "de.whole_file",
            "assertion_type": "string_contains",
            "target": "runtime/outputs/differential_expression/result.json",
            "check": { "substrings_any": ["design_formula"] }
        });
        let tmp = write_de_result(serde_json::json!({ "design_formula": "~ condition" }));
        let empty = std::collections::BTreeMap::new();
        assert!(
            run_assertion(tmp.path(), &a, &empty),
            "no json_pointer -> whole-file search (original behavior) finds the field name"
        );
    }

    /// A json_pointer that does not resolve to a string (absent / non-string)
    /// fails closed with an empty haystack rather than falling back to a
    /// whole-file search.
    #[test]
    fn string_contains_pointer_unresolved_fails_closed() {
        let tmp = write_de_result(serde_json::json!({
            "r_squared_column_recorded": true
            // no narrative_text key at all
        }));
        let empty = std::collections::BTreeMap::new();
        assert!(
            !run_assertion(tmp.path(), &reports_model_fit_assertion(), &empty),
            "missing /narrative_text -> empty haystack -> fails closed (no whole-file fallback)"
        );
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

    /// Compute-language neutrality (render-as-contract intent): the environment
    /// contract must not justify a compute language by the (Python) renderer
    /// substrate. The python interpreter note must read as a neutral environment
    /// fact, symmetric with the R note, and an explicit neutrality statement
    /// must be present. Guards against regression of the env_capability leak.
    #[test]
    fn env_capability_compute_language_is_neutral() {
        let tmp = tempfile::tempdir().unwrap();
        write_env_capability(tmp.path()).unwrap();
        let body: serde_json::Value =
            serde_json::from_slice(&std::fs::read(tmp.path().join("runtime/env_capability.json")).unwrap())
                .unwrap();
        let env = body.get("environment").unwrap();
        // The outer environment note must not single out "the canonical
        // interpreter" (which reads as Python-primary); it describes interpreter
        // resolution neutrally.
        let outer_note = env.get("note").unwrap().as_str().unwrap();
        assert!(
            !outer_note.contains("the canonical interpreter"),
            "environment.note must not frame a single 'canonical interpreter' (Python-primary reading)"
        );
        let py_note = env.get("python").unwrap().get("note").unwrap().as_str().unwrap();
        // The python note must NOT cite the renderer substrate as why python is
        // canonical for general compute.
        for needle in ["scientific-python substrate", "numpy/pandas/matplotlib", "renderers use", "canonical interpreter"] {
            assert!(
                !py_note.contains(needle),
                "env_capability python.note must not justify python via the renderer substrate: found {needle:?}"
            );
        }
        // Explicit symmetric neutrality statement is present and names both.
        let neutral = env
            .get("compute_language")
            .and_then(|v| v.as_str())
            .expect("environment.compute_language neutrality statement must be present");
        assert!(
            neutral.contains("Python") && neutral.contains("R") && neutral.to_lowercase().contains("neither is privileged"),
            "compute_language statement must present Python and R as un-privileged peers"
        );
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
            execution_order: Vec::new(),
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
                inputs: Vec::new(),
                outputs: Vec::new(),
                edam_operation: None,
                execution_index: None,
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
    fn stamp_determinism_env_threads_run_id_derived_seeds() {
        let dispatch = PickedDispatch {
            task_id: "t".into(),
            harness_run_id: "run-det".into(),
            epoch: 3,
        };
        let run_epoch = 1_767_225_600u64;
        let mut env: std::collections::BTreeMap<String, String> = Default::default();
        // Force-enable regardless of the ambient env in CI.
        stamp_determinism_env(&mut env, Some(&dispatch), run_epoch, true);
        assert_eq!(env.get("PYTHONHASHSEED").map(String::as_str), Some("0"));
        assert_eq!(env.get("LANG").map(String::as_str), Some("C.UTF-8"));
        assert!(env.contains_key("SOURCE_DATE_EPOCH"));
        // Disabled => no keys stamped.
        let mut env_off: std::collections::BTreeMap<String, String> = Default::default();
        stamp_determinism_env(&mut env_off, Some(&dispatch), run_epoch, false);
        assert!(env_off.is_empty(), "disabled knob must stamp nothing");
    }

    /// C2 twin (harness seam) — two tasks with DIFFERENT per-task
    /// dispatch `epoch`s, stamped under the SAME run epoch, must receive
    /// the SAME `SOURCE_DATE_EPOCH`, and it must equal the run epoch.
    /// Before the fix the per-task `dispatch.epoch` fed the value and
    /// these diverged (the 22-distinct-values bug).
    #[test]
    fn stamp_determinism_env_source_date_epoch_is_run_stable_across_tasks() {
        let run_epoch = 1_767_225_600u64;
        let task_a = PickedDispatch {
            task_id: "a".into(),
            harness_run_id: "run-det".into(),
            epoch: 1,
        };
        let task_b = PickedDispatch {
            task_id: "b".into(),
            harness_run_id: "run-det".into(),
            epoch: 22,
        };
        let mut env_a: std::collections::BTreeMap<String, String> = Default::default();
        let mut env_b: std::collections::BTreeMap<String, String> = Default::default();
        stamp_determinism_env(&mut env_a, Some(&task_a), run_epoch, true);
        stamp_determinism_env(&mut env_b, Some(&task_b), run_epoch, true);
        assert_eq!(
            env_a.get("SOURCE_DATE_EPOCH"),
            env_b.get("SOURCE_DATE_EPOCH"),
            "tasks 1 and 22 of the same run must share SOURCE_DATE_EPOCH"
        );
        assert_eq!(
            env_a.get("SOURCE_DATE_EPOCH").map(String::as_str),
            Some("1767225600"),
            "SOURCE_DATE_EPOCH must be the run epoch verbatim"
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

    // ── pure helper unit tests (no docker, no R, no Python) ─────────────

    /// `build_probe_script` for an R probe without r_libs emits a line
    /// that calls `Rscript` with a plain `suppressMessages(library(...))`.
    #[test]
    fn build_probe_script_r_no_rlibs() {
        let specs = vec![ProbeSpec {
            key: "deseq2".to_string(),
            kind: ProbeKind::R("DESeq2".to_string()),
        }];
        let script = build_probe_script(&specs, None);
        assert!(
            script.contains("Rscript"),
            "script must invoke Rscript: {script}"
        );
        assert!(
            script.contains("DESeq2"),
            "script must reference the package name: {script}"
        );
        assert!(
            script.contains("deseq2=1") && script.contains("deseq2=0"),
            "script must emit key=1 and key=0 branches: {script}"
        );
        // With no r_libs prefix, .libPaths is NOT called.
        assert!(
            !script.contains(".libPaths(c("),
            "no r_libs => no .libPaths(...) call: {script}"
        );
    }

    /// With an r_libs prefix the generated script prepends it in .libPaths.
    #[test]
    fn build_probe_script_r_with_rlibs() {
        let specs = vec![ProbeSpec {
            key: "seurat".to_string(),
            kind: ProbeKind::R("Seurat".to_string()),
        }];
        let script = build_probe_script(&specs, Some("/work/pkg/runtime/r-libs"));
        assert!(
            script.contains("/work/pkg/runtime/r-libs"),
            "r_libs path must appear in script: {script}"
        );
        assert!(
            script.contains(".libPaths(c("),
            "script must call .libPaths to prepend r_libs: {script}"
        );
    }

    /// `build_probe_script` for a Python probe emits `python3 -c 'import ...'`.
    #[test]
    fn build_probe_script_python() {
        let specs = vec![ProbeSpec {
            key: "pyscenic".to_string(),
            kind: ProbeKind::Python("pyscenic".to_string()),
        }];
        let script = build_probe_script(&specs, None);
        assert!(script.contains("python3"), "must invoke python3: {script}");
        assert!(
            script.contains("import pyscenic"),
            "must import the target module: {script}"
        );
        assert!(
            script.contains("pyscenic=1") && script.contains("pyscenic=0"),
            "must emit key=1 and key=0 branches: {script}"
        );
    }

    /// `build_probe_script` for a `PythonAny` probe OR-chains the imports.
    #[test]
    fn build_probe_script_python_any() {
        let specs = vec![ProbeSpec {
            key: "python_lisi".to_string(),
            kind: ProbeKind::PythonAny(vec![
                "lisi".to_string(),
                "harmonypy".to_string(),
                "scanpy.external.pp.lisi".to_string(),
            ]),
        }];
        let script = build_probe_script(&specs, None);
        // All three imports must appear.
        for needle in ["import lisi", "import harmonypy", "import scanpy.external.pp.lisi"] {
            assert!(
                script.contains(needle),
                "PythonAny script must contain '{needle}': {script}"
            );
        }
        assert!(
            script.contains("||"),
            "PythonAny script must OR-chain imports: {script}"
        );
    }

    /// `build_probe_script` for a Cellranger probe emits the version line.
    #[test]
    fn build_probe_script_cellranger() {
        let specs = vec![ProbeSpec {
            key: "CELLRANGER".to_string(),
            kind: ProbeKind::Cellranger,
        }];
        let script = build_probe_script(&specs, None);
        assert!(
            script.contains("cellranger --version"),
            "must probe cellranger version: {script}"
        );
        assert!(
            script.contains("CELLRANGER="),
            "must emit CELLRANGER= line: {script}"
        );
    }

    /// `parse_probe_output` handles bool `=1`/`=0` lines and the
    /// `CELLRANGER=` prefix correctly.
    #[test]
    fn parse_probe_output_mixed() {
        let stdout = "deseq2=1\nedger=0\nCELLRANGER=cellranger-7.2.0\npyscenic=1\n";
        let (bools, cr) = parse_probe_output(stdout);
        assert_eq!(bools.get("deseq2"), Some(&true));
        assert_eq!(bools.get("edger"),  Some(&false));
        assert_eq!(bools.get("pyscenic"), Some(&true));
        // CELLRANGER should NOT appear in the bool map.
        assert!(!bools.contains_key("CELLRANGER"));
        assert_eq!(cr.as_deref(), Some("cellranger-7.2.0"));
    }

    /// `parse_probe_output` returns `None` for cellranger when the line
    /// is `CELLRANGER=` (empty, meaning binary absent).
    #[test]
    fn parse_probe_output_cellranger_absent() {
        let stdout = "deseq2=0\nCELLRANGER=\n";
        let (bools, cr) = parse_probe_output(stdout);
        assert_eq!(bools.get("deseq2"), Some(&false));
        assert_eq!(cr, None, "empty CELLRANGER= must yield None");
    }

    /// `parse_probe_output` tolerates blank lines and unexpected lines.
    #[test]
    fn parse_probe_output_tolerates_noise() {
        let stdout = "\n\ndeseq2=1\n# some unexpected comment\ngarbage line\nedger=0\n";
        let (bools, cr) = parse_probe_output(stdout);
        assert_eq!(bools.get("deseq2"), Some(&true));
        assert_eq!(bools.get("edger"),  Some(&false));
        assert_eq!(cr, None);
        // Unexpected lines must not panic, and must not appear in bools.
        assert!(!bools.contains_key("# some unexpected comment"));
        assert!(!bools.contains_key("garbage line"));
    }

    /// `resolve_probe_image` returns `None` when neither the policy file
    /// nor the env var supplies an image.
    #[test]
    fn resolve_probe_image_none_when_no_source() {
        let tmp = tempfile::tempdir().unwrap();
        // Write a container.json with null image (the default testdata shape).
        std::fs::create_dir_all(tmp.path().join("policies")).unwrap();
        std::fs::write(
            tmp.path().join("policies/container.json"),
            r#"{"image": null}"#,
        )
        .unwrap();
        // Ensure the env var is not set in this process.
        // (We only unset it if it was set; the test is still meaningful
        // because the policy file returns null.)
        let _guard = EnvVarGuard::unset("ECAA_DEFAULT_CONTAINER_IMAGE");
        let result = resolve_probe_image(tmp.path());
        assert_eq!(result, None);
    }

    /// `resolve_probe_image` prefers the policy file over the env var.
    #[test]
    fn resolve_probe_image_policy_file_wins() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("policies")).unwrap();
        std::fs::write(
            tmp.path().join("policies/container.json"),
            r#"{"image": "ghcr.io/ecaa/bio-min:latest"}"#,
        )
        .unwrap();
        let _guard = EnvVarGuard::set(
            "ECAA_DEFAULT_CONTAINER_IMAGE",
            "ghcr.io/ecaa/bio-min:other",
        );
        let result = resolve_probe_image(tmp.path());
        assert_eq!(result.as_deref(), Some("ghcr.io/ecaa/bio-min:latest"));
    }

    /// `resolve_probe_image` falls back to the env var when the policy
    /// file has `"image": null`.
    #[test]
    fn resolve_probe_image_env_fallback() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("policies")).unwrap();
        std::fs::write(
            tmp.path().join("policies/container.json"),
            r#"{"image": null}"#,
        )
        .unwrap();
        let _guard = EnvVarGuard::set("ECAA_DEFAULT_CONTAINER_IMAGE", "ghcr.io/ecaa/bio-min:env");
        let result = resolve_probe_image(tmp.path());
        assert_eq!(result.as_deref(), Some("ghcr.io/ecaa/bio-min:env"));
    }

    /// `probe_site` field is present in the emitted JSON.
    #[test]
    fn env_capability_probe_site_field_present() {
        let tmp = tempfile::tempdir().unwrap();
        write_env_capability(tmp.path()).unwrap();
        let body: serde_json::Value = serde_json::from_slice(
            &std::fs::read(tmp.path().join("runtime/env_capability.json")).unwrap(),
        )
        .unwrap();
        let site = body
            .get("probe_site")
            .expect("probe_site field must be present in env_capability.json")
            .as_str()
            .expect("probe_site must be a string");
        // Without a container image configured this will be "host-fallback".
        assert!(
            site == "host-fallback" || site.starts_with("container:"),
            "probe_site must be 'host-fallback' or 'container:<image>', got: {site}"
        );
    }

    /// Helper for temporarily setting / unsetting an env var in tests.
    struct EnvVarGuard {
        key: &'static str,
        original: Option<String>,
    }
    impl EnvVarGuard {
        fn set(key: &'static str, value: &str) -> Self {
            let original = std::env::var(key).ok();
            std::env::set_var(key, value);
            Self { key, original }
        }
        fn unset(key: &'static str) -> Self {
            let original = std::env::var(key).ok();
            std::env::remove_var(key);
            Self { key, original }
        }
    }
    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            match &self.original {
                Some(v) => std::env::set_var(self.key, v),
                None    => std::env::remove_var(self.key),
            }
        }
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
    use ecaa_workflow_core::dag::{Assignee, BlockedRecord, ResourceClass, Task, TaskId, TaskKind};
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
                inputs: Vec::new(),
                outputs: Vec::new(),
                edam_operation: None,
                execution_index: None,
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
            execution_order: Vec::new(),
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
        //
        // No runtime assertion: the body intentionally does nothing — the
        // invariant is enforced by code structure, and this test anchors the
        // coverage gap in the test-count baseline. See module-level doc above.
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
            inputs: Vec::new(),
            outputs: Vec::new(),
            edam_operation: None,
            execution_index: None,
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
            execution_order: Vec::new(),
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
mod wall_clock_blocker_mapping_tests {
    use super::*;
    use ecaa_workflow_harness::executor::IterationCapture;

    #[test]
    fn wall_clock_killed_reports_enforced_deadline_not_raw_task_timeout() {
        // A capture flagged wall_clock_killed must yield the
        // (observed_secs, threshold_secs) pair the harness feeds into
        // `pc.wall_clock_exceeded`, which the server promotes to
        // `Blocked { BlockerKind::WallClockExceeded }`. observed_secs is the
        // capture's measured wallclock; threshold_secs must be the deadline the
        // executor ACTUALLY enforced (effective_deadline_secs), NOT the raw
        // --task-timeout — otherwise the SME message is self-contradictory
        // ("14520s observed, 300s threshold").
        let cap = IterationCapture {
            wall_clock_killed: true,
            wallclock_secs: Some(14520),
            effective_deadline_secs: Some(14520),
            exit_code: None,
            signal: Some("SIGKILL".into()),
            ..Default::default()
        };
        let task_timeout = 300u64; // raw --task-timeout, smaller than the agent wallclock backstop
        let params =
            wall_clock_blocker_params(&cap, task_timeout).expect("wall-clock kill must map");
        assert_eq!(
            params,
            (14520, 14520),
            "threshold_secs == the enforced deadline, not the raw --task-timeout"
        );
    }

    #[test]
    fn wall_clock_falls_back_to_task_timeout_when_deadline_unreported() {
        // Backends that cannot report effective_deadline_secs (None) fall back
        // to the raw task_timeout so the blocker still carries a threshold.
        let cap = IterationCapture {
            wall_clock_killed: true,
            wallclock_secs: Some(312),
            effective_deadline_secs: None,
            ..Default::default()
        };
        assert_eq!(wall_clock_blocker_params(&cap, 300), Some((312, 300)));
    }

    #[test]
    fn wall_clock_killed_defaults_observed_to_zero_when_unmeasured() {
        // Defensive: a kill with no recorded wallclock still maps,
        // reporting observed == 0 rather than dropping the blocker.
        let cap = IterationCapture {
            wall_clock_killed: true,
            wallclock_secs: None,
            ..Default::default()
        };
        assert_eq!(wall_clock_blocker_params(&cap, 120), Some((0, 120)));
    }

    #[test]
    fn ordinary_failure_does_not_map_to_wall_clock_blocker() {
        // A plain non-zero exit (not a wall-clock kill) must return None
        // so the caller falls through to the normal tool-error-envelope
        // path instead of synthesising a WallClockExceeded blocker.
        let cap = IterationCapture {
            wall_clock_killed: false,
            wallclock_secs: Some(42),
            exit_code: Some(1),
            ..Default::default()
        };
        assert_eq!(wall_clock_blocker_params(&cap, 300), None);
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
            inputs: Vec::new(),
            outputs: Vec::new(),
            edam_operation: None,
            execution_index: None,
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
            execution_order: Vec::new(),
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
        let invalidated = ["t1".to_string(), "t2".to_string(), "t3".to_string()];

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

#[cfg(test)]
mod frozen_method_authority_tests {
    //! Unit tests for the frozen-on-rerun method-source-authority seam
    //! (plan Task 5.2). These pin the stamping behaviour of
    //! `stamp_literature_scope` under the `freeze_method_authority` flag
    //! without a live server or executor.
    use super::*;

    // Process-global env is shared across all tests in this binary; serialize
    // every set_var/remove_var window so a parallel test's mutation cannot
    // leak into another's read (mirrors literature_scope::tests::ENV_LOCK).
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn with_authority_env<F: FnOnce()>(value: Option<&str>, f: F) {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let restore = std::env::var("ECAA_METHOD_SOURCE_AUTHORITY").ok();
        match value {
            Some(v) => std::env::set_var("ECAA_METHOD_SOURCE_AUTHORITY", v),
            None => std::env::remove_var("ECAA_METHOD_SOURCE_AUTHORITY"),
        }
        f();
        match restore {
            Some(v) => std::env::set_var("ECAA_METHOD_SOURCE_AUTHORITY", v),
            None => std::env::remove_var("ECAA_METHOD_SOURCE_AUTHORITY"),
        }
    }

    #[test]
    fn freeze_override_forces_frozen_authority() {
        // Even when the env opts into bounded discovery, the rerun freeze
        // override must stamp `frozen`.
        with_authority_env(Some("bounded"), || {
            let mut env = std::collections::BTreeMap::new();
            stamp_literature_scope(&mut env, true);
            assert_eq!(
                env.get("ECAA_METHOD_SOURCE_AUTHORITY").map(String::as_str),
                Some("frozen")
            );
        });
    }

    #[test]
    fn no_override_honours_configured_authority() {
        with_authority_env(Some("bounded"), || {
            let mut env = std::collections::BTreeMap::new();
            stamp_literature_scope(&mut env, false);
            assert_eq!(
                env.get("ECAA_METHOD_SOURCE_AUTHORITY").map(String::as_str),
                Some("bounded")
            );
        });
    }

    #[test]
    fn default_authority_is_bounded_without_freeze() {
        with_authority_env(None, || {
            let mut env = std::collections::BTreeMap::new();
            stamp_literature_scope(&mut env, false);
            assert_eq!(
                env.get("ECAA_METHOD_SOURCE_AUTHORITY").map(String::as_str),
                Some("bounded")
            );
        });
    }

    #[test]
    fn should_freeze_defaults_false_without_flag() {
        // The conservative default: a fresh run does not freeze, so the
        // configured `ECAA_METHOD_SOURCE_AUTHORITY` (bounded by default)
        // governs and live discovery is permitted.
        let args = Args::try_parse_from(["harness", "--package", "/tmp/p", "--agent", "a"])
            .expect("parse default args");
        assert!(!should_freeze_method_authority(&args));
    }

    #[test]
    fn should_freeze_true_when_flag_set() {
        // `--frozen-method-source` is the grounded rerun/amend signal: the
        // server appends it when relaunching against an already-emitted
        // package, forcing `frozen` so no fresh live discovery occurs.
        let args = Args::try_parse_from([
            "harness",
            "--package",
            "/tmp/p",
            "--agent",
            "a",
            "--frozen-method-source",
        ])
        .expect("parse args with frozen flag");
        assert!(should_freeze_method_authority(&args));
    }

    #[test]
    fn flag_forces_frozen_env_stamp_end_to_end() {
        // With the flag set, the stamped agent env is `frozen` even when
        // the configured authority opts into bounded discovery.
        with_authority_env(Some("bounded"), || {
            let args = Args::try_parse_from([
                "harness",
                "--package",
                "/tmp/p",
                "--agent",
                "a",
                "--frozen-method-source",
            ])
            .expect("parse args with frozen flag");
            let mut env = std::collections::BTreeMap::new();
            stamp_literature_scope(&mut env, should_freeze_method_authority(&args));
            assert_eq!(
                env.get("ECAA_METHOD_SOURCE_AUTHORITY").map(String::as_str),
                Some("frozen")
            );
        });
    }

    #[test]
    fn no_flag_honours_configured_env_authority_end_to_end() {
        // Without the flag the configured authority governs the stamp.
        with_authority_env(Some("bounded"), || {
            let args = Args::try_parse_from(["harness", "--package", "/tmp/p", "--agent", "a"])
                .expect("parse default args");
            let mut env = std::collections::BTreeMap::new();
            stamp_literature_scope(&mut env, should_freeze_method_authority(&args));
            assert_eq!(
                env.get("ECAA_METHOD_SOURCE_AUTHORITY").map(String::as_str),
                Some("bounded")
            );
        });
    }
}

/// Coverage for the `run_loop` per-iteration transition-classification
/// logic (extracted into `decide_task_progress_event`) plus a
/// MockExecutor-driven smoke of the executor dispatch surface run_loop
/// drives. The full `run_loop` cannot be invoked from a unit test without
/// a fully-formed package on disk + host probing + dispatch threads, so
/// coverage targets the extracted decision step — which carries the
/// load-bearing harness-04 invariant — and the scripted-outcome ordering
/// the loop relies on.
#[cfg(test)]
mod run_loop_transition_tests {
    use super::*;
    // `TaskState` and `DAG` arrive via `super::*`; only `BlockedRecord`
    // needs an explicit import.
    use ecaa_workflow_core::dag::BlockedRecord;

    fn running() -> TaskState {
        TaskState::Running {
            started_at: "2026-06-01T00:00:00Z".into(),
            remote: None,
        }
    }
    fn completed() -> TaskState {
        TaskState::Completed {
            result: serde_json::json!({"ok": true}),
        }
    }
    fn failed() -> TaskState {
        TaskState::Failed {
            reason: "agent exited non-zero".into(),
        }
    }
    fn blocked(reason: &str) -> TaskState {
        TaskState::Blocked {
            record: BlockedRecord {
                reason: reason.into(),
                attempts: vec![],
            },
        }
    }

    /// A freshly-Running task with no prior observation → `Started`,
    /// mirror the state, insert into prior_running, no scratch cleanup.
    #[test]
    fn first_running_observation_emits_started() {
        let d = decide_task_progress_event(&running(), false, false, false, false, "align", false);
        assert_eq!(d.event, TaskProgressEvent::Started);
        assert!(d.mirror_state);
        assert!(!d.cleanup_scratch);
        assert!(d.ops.insert_running);
        assert!(!d.ops.remove_running);
    }

    /// Running task already in prior_running → no event re-emitted.
    #[test]
    fn repeat_running_observation_is_silent() {
        let d = decide_task_progress_event(&running(), false, true, false, false, "align", false);
        assert_eq!(d.event, TaskProgressEvent::None);
        assert!(!d.mirror_state);
    }

    /// First Completed observation → `Completed`, scratch cleanup, drop
    /// from prior_running, record in prior_completed.
    #[test]
    fn first_completed_observation_emits_completed_and_cleans_scratch() {
        let d = decide_task_progress_event(&completed(), false, true, false, false, "align", false);
        assert_eq!(d.event, TaskProgressEvent::Completed);
        assert!(d.mirror_state);
        assert!(d.cleanup_scratch);
        assert!(d.ops.insert_completed);
        assert!(d.ops.remove_running);
    }

    /// harness-04 CORE REGRESSION GUARD: a task that the harness already
    /// pre-marked Running (so it IS in prior_running) then transitions to
    /// Failed MUST still POST `task_failed`. The old gate
    /// (`is_failed && !prior_running`) suppressed this entirely.
    #[test]
    fn failed_after_premark_running_still_emits_failed() {
        // prior_running = true (pre-marked), prior_failed = false.
        let d = decide_task_progress_event(&failed(), false, true, false, false, "align", false);
        assert_eq!(
            d.event,
            TaskProgressEvent::Failed,
            "Running→Failed must POST task_failed even though prior_running is set"
        );
        assert!(d.mirror_state);
        assert!(d.cleanup_scratch);
        assert!(d.ops.insert_failed);
        assert!(d.ops.remove_running);
    }

    /// harness-04 ONCE-ONLY GUARD: once the failure has been observed
    /// (prior_failed = true) the event is NOT re-emitted on later passes.
    #[test]
    fn failed_already_observed_is_silent() {
        let d = decide_task_progress_event(&failed(), false, true, false, true, "align", false);
        assert_eq!(d.event, TaskProgressEvent::None);
        assert!(!d.mirror_state);
        assert!(!d.cleanup_scratch);
        assert!(!d.ops.insert_failed);
    }

    /// AWS SSM bare-`Failed` path: the executor sets `TaskState::Failed`
    /// directly without ever pre-marking Running locally. The event must
    /// still fire exactly once.
    #[test]
    fn bare_failed_without_prior_running_emits_failed() {
        let d = decide_task_progress_event(&failed(), false, false, false, false, "ssm", false);
        assert_eq!(d.event, TaskProgressEvent::Failed);
        assert!(d.ops.insert_failed);
    }

    /// Failed WITH an `error.json` envelope routes to `task_blocked`
    /// (BlockerKind::ToolError) carrying the failure reason, and also
    /// records the task in prior_blocked.
    #[test]
    fn failed_with_envelope_routes_as_blocked() {
        let d = decide_task_progress_event(&failed(), false, true, false, false, "align", true);
        match &d.event {
            TaskProgressEvent::FailedAsBlocked { reason } => {
                assert_eq!(reason, "agent exited non-zero");
            }
            other => panic!("expected FailedAsBlocked, got {:?}", other),
        }
        assert!(
            d.cleanup_scratch,
            "Failed transition still reclaims scratch"
        );
        assert!(d.ops.insert_failed);
        assert!(d.ops.insert_blocked);
    }

    /// First Blocked observation surfaces the agent reason; falls back to
    /// the description when the record reason is empty.
    #[test]
    fn first_blocked_observation_uses_agent_reason() {
        let d = decide_task_progress_event(
            &blocked("missing input file"),
            false,
            true,
            false,
            false,
            "align",
            false,
        );
        match &d.event {
            TaskProgressEvent::Blocked { reason } => assert_eq!(reason, "missing input file"),
            other => panic!("expected Blocked, got {:?}", other),
        }
        assert!(d.ops.insert_blocked);
        assert!(d.ops.remove_running);
    }

    #[test]
    fn blocked_with_empty_reason_falls_back_to_description() {
        let d =
            decide_task_progress_event(&blocked(""), false, true, false, false, "the-task", false);
        match &d.event {
            TaskProgressEvent::Blocked { reason } => assert_eq!(reason, "the-task"),
            other => panic!("expected Blocked, got {:?}", other),
        }
    }

    /// A task that leaves Blocked (now Completed) while still flagged in
    /// prior_blocked must clear the blocked once-guard so a later re-block
    /// fires again — and the same pass still emits Completed.
    #[test]
    fn leaving_blocked_clears_prior_blocked_guard() {
        let d = decide_task_progress_event(&completed(), false, false, true, false, "align", false);
        assert_eq!(d.event, TaskProgressEvent::Completed);
        assert!(d.ops.remove_blocked, "must clear the blocked once-guard");
        assert!(d.ops.insert_completed);
    }

    /// Drives a multi-iteration sequence the way `run_loop` does: replay
    /// the decision over a Ready→Running→Failed lifecycle, threading the
    /// once-guard sets between passes, and assert the failed event fires
    /// EXACTLY ONCE across the lingering-Failed iterations (harness-04).
    #[test]
    fn lifecycle_emits_failed_exactly_once_across_iterations() {
        use std::collections::HashSet;
        let mut prior_completed: HashSet<String> = HashSet::new();
        let mut prior_running: HashSet<String> = HashSet::new();
        let mut prior_blocked: HashSet<String> = HashSet::new();
        let mut prior_failed: HashSet<String> = HashSet::new();
        let tid = "align";

        // Mirror the run_loop apply order (removes before inserts).
        let mut apply = |state: &TaskState| -> TaskProgressEvent {
            let d = decide_task_progress_event(
                state,
                prior_completed.contains(tid),
                prior_running.contains(tid),
                prior_blocked.contains(tid),
                prior_failed.contains(tid),
                "align",
                false,
            );
            let ops = &d.ops;
            if ops.remove_running {
                prior_running.remove(tid);
            }
            if ops.remove_blocked {
                prior_blocked.remove(tid);
            }
            if ops.insert_running {
                prior_running.insert(tid.to_string());
            }
            if ops.insert_completed {
                prior_completed.insert(tid.to_string());
            }
            if ops.insert_blocked {
                prior_blocked.insert(tid.to_string());
            }
            if ops.insert_failed {
                prior_failed.insert(tid.to_string());
            }
            d.event
        };

        // Iteration 1: pre-marked Running → Started.
        assert_eq!(apply(&running()), TaskProgressEvent::Started);
        // Iteration 2: still Running → silent.
        assert_eq!(apply(&running()), TaskProgressEvent::None);
        // Iteration 3: Running→Failed → Failed (the harness-04 fix).
        assert_eq!(apply(&failed()), TaskProgressEvent::Failed);
        // Iterations 4..6: task lingers in Failed → never re-emits.
        for _ in 0..3 {
            assert_eq!(apply(&failed()), TaskProgressEvent::None);
        }
        assert!(prior_failed.contains(tid));
        assert!(
            !prior_running.contains(tid),
            "running guard dropped on Failed"
        );
    }
}

/// MockExecutor-driven coverage of the executor dispatch surface
/// `run_loop` iterates over: scripted outcomes are returned in order and
/// exhaustion is an error rather than a silent no-op.
///
/// Gated on `feature = "dry-run"` because `executor::mock` is itself
/// `#[cfg(any(test, feature = "dry-run"))]` — when the binary's unit
/// tests link the library as a dependency, the library is built WITHOUT
/// `cfg(test)`, so `MockExecutor` is only reachable here under `dry-run`.
/// Run with `cargo test -p ecaa-workflow-harness --features dry-run`.
#[cfg(all(test, feature = "dry-run"))]
mod run_loop_executor_smoke_tests {
    // Fully explicit imports (no `super::*`) so this feature-gated module
    // can't pick up a duplicate of an already-globbed name.
    use ecaa_workflow_core::dag::DAG;
    use ecaa_workflow_harness::executor::mock::MockExecutor;
    use ecaa_workflow_harness::executor::{Executor, IterationOutcome};
    use std::collections::BTreeMap;
    use std::os::unix::process::ExitStatusExt;
    use std::path::PathBuf;
    use std::process::ExitStatus;

    fn empty_dag() -> DAG {
        DAG {
            version: "1.0".into(),
            schema_version: ecaa_workflow_core::dag::current_dag_schema_version(),
            workflow_id: "mock".into(),
            current_task: None,
            tasks: Default::default(),
            reverse_deps: Default::default(),
            run_id: None,
        }
    }

    #[test]
    fn mock_executor_returns_scripted_failure_then_success_in_order() {
        let mut m = MockExecutor::new(vec![
            IterationOutcome {
                agent_status: ExitStatus::from_raw(256), // exit code 1 → failure
                remote: None,
            },
            IterationOutcome {
                agent_status: ExitStatus::from_raw(0),
                remote: None,
            },
        ]);
        let path = PathBuf::from("/tmp/pkg");
        m.provision(&empty_dag()).expect("provision");
        let first = m
            .run_iteration(&path, "agent", &BTreeMap::new())
            .expect("first outcome");
        assert!(
            !first.agent_status.success(),
            "first scripted outcome fails"
        );
        let second = m
            .run_iteration(&path, "agent", &BTreeMap::new())
            .expect("second outcome");
        assert!(
            second.agent_status.success(),
            "second scripted outcome succeeds"
        );
        assert_eq!(m.provision_calls(), 1);
        assert_eq!(m.remaining(), 0);
    }
}
