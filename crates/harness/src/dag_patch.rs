//! Per-task state-patch protocol — the agent writes its proposed
//! transition to `runtime/outputs/<task_id>/state.patch.json`, the
//! harness merges patches at iteration end. The agent must never
//! edit `WORKFLOW.json` directly; only the harness merges patches.
//!
//! Startup recovery remains opportunistic for compatibility with older
//! packages: valid orphan patches are merged when their `from` guard
//! matches the live DAG. Normal dispatch harvesting is stricter: a
//! patch must belong to the exact task, harness run, and dispatch epoch
//! the harness launched in this iteration.
//!
//! Why: under the old protocol, two parallel agents both running
//! `read → mutate → write` on `WORKFLOW.json` clobber each other; the
//! last writer's snapshot wins and one task transition is silently
//! lost. Per-task patch files are a per-task lock by construction —
//! agents only ever touch their own task's output directory, and the
//! harness is the only writer to `WORKFLOW.json`.

use anyhow::{Context, Result};
use ecaa_workflow_core::blocker::BlockerKind;
use ecaa_workflow_core::dag::{BlockedRecord, TaskId, TaskState, DAG};
use serde::{Deserialize, Serialize};
use std::path::Path;

use crate::ecaa_io::{read_capped, resolve_max_bytes};

/// Current on-disk schema version for [`StatePatch`]. Callers that need the
/// canonical version constant without constructing a patch should call this
/// directly; the `#[serde(default)]` annotation on the field delegates here.
pub fn state_patch_schema_version() -> semver::Version {
    ecaa_workflow_core::migration::current_state_patch_version()
}

fn default_state_patch_schema_version() -> semver::Version {
    state_patch_schema_version()
}

/// Dispatch identity stamped into the agent envelope and expected back
/// in `state.patch.json` for normal live dispatch harvesting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PickedDispatch {
    /// Task that was dispatched.
    pub task_id: TaskId,
    /// Per-process run id stamped at harness startup for crash-detection.
    pub harness_run_id: String,
    /// Monotonically-increasing dispatch counter within this harness run.
    pub epoch: u64,
}

/// One agent's proposed state transition for a single task. Lives at
/// `runtime/outputs/<task_id>/state.patch.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatePatch {
    /// On-disk schema version. `#[serde(default)]` returns `0.1.0` for
    /// patch files written before this field was added, so older packages
    /// continue to load. The `schema_version_serde` adapter accepts both
    /// legacy `u64` values and canonical SemVer strings on read; writes
    /// the canonical SemVer string.
    #[serde(
        default = "default_state_patch_schema_version",
        with = "ecaa_workflow_core::migration::schema_version_serde"
    )]
    pub schema_version: semver::Version,
    /// Optional sanity check: expected current status tag (e.g.
    /// `"running"`). When present and the live task's status differs,
    /// the patch is skipped with a `[patch] from-mismatch` warning so
    /// a stale patch from a prior run can't reset a Completed task.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    /// Harness process id that launched this task. Required by
    /// [`apply_pending_patches_strict`], optional for legacy startup
    /// orphan recovery via [`apply_pending_patches`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub harness_run_id: Option<String>,
    /// Per-run dispatch epoch that launched this task. Required by
    /// [`apply_pending_patches_strict`], optional for legacy startup
    /// orphan recovery via [`apply_pending_patches`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dispatch_epoch: Option<u64>,
    /// Replacement state — uses the canonical `TaskState` tag layout
    /// (`{"status": "completed", "result":...}` etc.). Accepted
    /// terminals from agents are `completed`, `blocked`, and `failed`.
    /// `running` is also accepted IFF the live task is already
    /// `Running` — that arm is a structural no-op "still in flight"
    /// heartbeat patch (see [`apply_pending_patches`]); the live
    /// state's `started_at` and `remote` are preserved verbatim.
    pub to: TaskState,
    /// Optional free-form note. When `to` is `Running` (a no-op
    /// heartbeat patch confirming compute is still in flight), the
    /// note is written to
    /// `runtime/outputs/<task_id>/last_agent_check.json` so a future
    /// SME or operator can audit the agent's evidence without re-running
    /// the LLM. Ignored for terminal transitions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// Read `WORKFLOW.json` and merge in any patch files found under
/// `runtime/outputs/<id>/state.patch.json` for the given pick set.
///
/// Returns the merged DAG without writing it back — the caller decides
/// when to persist (typically after running validation-contract
/// enforcement on the same in-memory DAG, then a single atomic write).
///
/// Patch files are *consumed* on successful application: once merged,
/// the file is renamed to `state.patch.applied.json` so subsequent
/// iterations don't re-apply a stale transition. A missing or
/// invalid patch is logged and falls through to the existing
/// `WORKFLOW.json` state — the legacy direct-write path remains
/// supported until the agent prompt rollout is complete.
///
/// When a patch file fails JSON parsing (as distinct from logical
/// rejection such as from-mismatch), the file is quarantined: renamed to
/// `state.patch.json.rejected-<compact_timestamp>` and the task is
/// transitioned to `Blocked { PatchUnparseable }` so the operator is
/// alerted. Logically-rejected patches (wrong from-state, dep guard,
/// etc.) remain on disk as before.
pub fn apply_pending_patches(package_dir: &Path, picks: &[TaskId]) -> Result<DAG> {
    let mut dag: DAG = read_workflow(package_dir)?;
    let mut applied: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();

    // Pass 1: explicit picks. The common path — the harness just
    // dispatched these tasks and is now harvesting their transitions.
    for task_id in picks {
        let patch_path = patch_path_for(package_dir, task_id.as_str());
        if !patch_path.exists() {
            continue;
        }
        match try_apply_patch(&mut dag, task_id.as_str(), &patch_path, None) {
            Ok(_) => {
                consume_patch(&patch_path, task_id.as_str());
                applied.insert(task_id.to_string());
            }
            Err(e) => {
                if let Some(rejected_path) =
                    maybe_quarantine_malformed(&patch_path, task_id.as_str(), &e)
                {
                    block_task_patch_unparseable(&mut dag, task_id.as_str(), &rejected_path, &e);
                }
            }
        }
    }

    // Pass 2: orphan scan. Pick up any `runtime/outputs/<id>/state.patch.json`
    // that survived a prior run (e.g. previous harness binary didn't
    // honor the patch protocol; agent crashed mid-iteration; merge
    // failed earlier). Apply only when the patch's `from` still
    // matches the live task state — the from-state guard in
    // try_apply_patch protects against re-applying a stale patch
    // whose intent has been superseded.
    let outputs_root = package_dir.join("runtime/outputs");
    if let Ok(entries) = std::fs::read_dir(&outputs_root) {
        for entry in entries.flatten() {
            let Ok(name) = entry.file_name().into_string() else {
                continue;
            };
            if applied.contains(&name) {
                continue;
            }
            if !entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                continue;
            }
            let patch_path = patch_path_for(package_dir, &name);
            if !patch_path.exists() {
                continue;
            }
            match try_apply_patch(&mut dag, &name, &patch_path, None) {
                Ok(_) => {
                    consume_patch(&patch_path, &name);
                    // Orphan patches typically signal a dispatch-identity
                    // mismatch (agent wrote `harness_run_id=""` or otherwise
                    // didn't carry the expected forward-keys). Surface as
                    // WARN so CI log greps catch silent identity drift —
                    // INFO traffic buries the signal otherwise.
                    tracing::warn!(
                        target: "patch",
                        task_id = %name,
                        "merged orphan state.patch.json (not in current picks) — possible dispatch-identity drift"
                    );
                }
                Err(e) => {
                    if let Some(rejected_path) = maybe_quarantine_malformed(&patch_path, &name, &e)
                    {
                        block_task_patch_unparseable(&mut dag, &name, &rejected_path, &e);
                    }
                }
            }
        }
    }
    Ok(dag)
}

/// Strict live-dispatch patch harvest. Only explicit dispatches are
/// considered; orphan patch files are ignored until startup/finalize
/// recovery calls [`apply_pending_patches`]. Each patch must echo the
/// dispatch identity carried in the agent envelope.
pub fn apply_pending_patches_strict(
    package_dir: &Path,
    dispatches: &[PickedDispatch],
) -> Result<DAG> {
    let mut dag: DAG = read_workflow(package_dir)?;

    for dispatch in dispatches {
        let patch_path = patch_path_for(package_dir, dispatch.task_id.as_str());
        if !patch_path.exists() {
            continue;
        }
        match try_apply_patch(
            &mut dag,
            dispatch.task_id.as_str(),
            &patch_path,
            Some((&dispatch.harness_run_id, dispatch.epoch)),
        ) {
            Ok(_) => consume_patch(&patch_path, dispatch.task_id.as_str()),
            Err(e) => {
                if let Some(rejected_path) =
                    maybe_quarantine_malformed(&patch_path, dispatch.task_id.as_str(), &e)
                {
                    block_task_patch_unparseable(
                        &mut dag,
                        dispatch.task_id.as_str(),
                        &rejected_path,
                        &e,
                    );
                }
            }
        }
    }

    Ok(dag)
}

/// Rename, don't delete. The rejected file preserves the agent's intent for
/// post-incident review and downstream remediation_proposer side-call.
///
/// Inspect whether the error originates from JSON parsing (as opposed to a
/// logical rejection such as from-mismatch or dep-guard). If it is a parse
/// failure, rename the bad file to `state.patch.json.rejected-<timestamp>`
/// and return the rejected path so the caller can block the task. Returns
/// `None` for all non-parse errors — those stay on disk as they do today for
/// debugging.
///
/// Rename is atomic on the same filesystem (as is always the case for a file
/// in `runtime/outputs/<task_id>/`). If rename fails, falls back to
/// copy + remove with a tracing::warn on copy failure. A rename or copy
/// failure is not fatal — the harness can still block the task even if the
/// file stays at its original path; the operator will see the error on stderr.
fn maybe_quarantine_malformed(
    patch_path: &Path,
    task_id: &str,
    err: &anyhow::Error,
) -> Option<std::path::PathBuf> {
    // Only quarantine genuine parse failures. Logical rejections (from-mismatch,
    // dep-not-completed, terminal-on-non-running, etc.) leave the file on disk
    // unchanged — consistent with existing behaviour and useful for debugging.
    if !is_parse_error(patch_path, err) {
        tracing::error!(
            task_id = task_id,
            patch_path = %patch_path.display(),
            error = %err,
            "[patch] skipped"
        );
        return None;
    }

    let ts = ecaa_workflow_core::time_helpers::now_compact_filename();
    let rejected_name = format!("state.patch.json.rejected-{}", ts);
    let rejected_path = patch_path
        .parent()
        .map(|p| p.join(&rejected_name))
        .unwrap_or_else(|| std::path::PathBuf::from(&rejected_name));

    // Fast path: atomic rename on the same filesystem.
    let rename_err = match std::fs::rename(patch_path, &rejected_path) {
        Ok(()) => {
            tracing::error!(
                task_id = task_id,
                rejected_path = %rejected_path.display(),
                parse_error = %err,
                "[patch] parse failure: file quarantined"
            );
            return Some(rejected_path);
        }
        Err(rename_err) => rename_err,
    };

    // Rename failed — fall back to copy+remove.
    tracing::warn!(
        task_id = task_id,
        patch_path = %patch_path.display(),
        rejected_path = %rejected_path.display(),
        rename_error = %rename_err,
        "[patch] rename failed; falling back to copy+remove"
    );
    quarantine_via_copy(patch_path, &rejected_path, task_id, err)
}

/// Copy+remove fallback for `maybe_quarantine_malformed` when the atomic
/// rename fails. On copy success the file is quarantined at `rejected_path`
/// (a failed remove of the original is non-fatal and intentionally ignored);
/// on copy failure the task is still blocked against the original path.
fn quarantine_via_copy(
    patch_path: &Path,
    rejected_path: &Path,
    task_id: &str,
    err: &anyhow::Error,
) -> Option<std::path::PathBuf> {
    match std::fs::copy(patch_path, rejected_path) {
        Ok(_) => {
            let _ = std::fs::remove_file(patch_path);
            tracing::error!(
                task_id = task_id,
                rejected_path = %rejected_path.display(),
                parse_error = %err,
                "[patch] parse failure: file quarantined via copy"
            );
            Some(rejected_path.to_path_buf())
        }
        Err(copy_err) => {
            tracing::warn!(
                task_id = task_id,
                patch_path = %patch_path.display(),
                copy_error = %copy_err,
                "[patch] quarantine copy failed; file left at original path"
            );
            // Still block the task even when we couldn't move the file;
            // return the original path so the blocker's rejected_path field
            // points at the best available location.
            tracing::error!(
                task_id = task_id,
                rejected_path = %patch_path.display(),
                parse_error = %err,
                "[patch] parse failure: task will be blocked (file not renamed)"
            );
            Some(patch_path.to_path_buf())
        }
    }
}

/// Inspect the error chain for a JSON parse failure on `patch_path`. Returns
/// `true` when the error is a serde_json deserialization error from reading
/// that specific file; `false` for logical rejections (anyhow::bail! from the
/// guards inside `try_apply_patch`).
fn is_parse_error(patch_path: &Path, err: &anyhow::Error) -> bool {
    // The error chain is: anyhow::Context("parsing <path>") → serde_json::Error.
    // Check the context message and root cause together so we don't mis-classify
    // a dep-guard bail!() that happens to mention "parsing" in its text.
    let context_hint = format!("parsing {}", patch_path.display());
    for cause in err.chain() {
        if cause.to_string().contains(&context_hint) {
            return true;
        }
    }
    false
}

/// Transition `task_id` in `dag` to `Blocked { PatchUnparseable { ... } }`.
/// Best-effort: if the task isn't found in the DAG (unknown task, race
/// against an amend), logs a warning and returns without failing.
fn block_task_patch_unparseable(
    dag: &mut DAG,
    task_id: &str,
    rejected_path: &Path,
    parse_error: &anyhow::Error,
) {
    let Some(task) = dag.tasks.get_mut(task_id) else {
        tracing::warn!(
            task_id = task_id,
            "[patch] PatchUnparseable block skipped: task not in DAG"
        );
        return;
    };
    let kind = BlockerKind::PatchUnparseable {
        task_id: task_id.to_string(),
        rejected_path: rejected_path.display().to_string(),
        parse_error: format!("{:#}", parse_error),
    };
    let reason =
        serde_json::to_string(&kind).unwrap_or_else(|_| format!("[patch_unparseable] {}", task_id));
    task.state = TaskState::Blocked {
        record: BlockedRecord {
            reason,
            attempts: vec![],
        },
    };
}

fn patch_path_for(package_dir: &Path, task_id: &str) -> std::path::PathBuf {
    package_dir
        .join("runtime/outputs")
        .join(task_id)
        .join("state.patch.json")
}

fn consume_patch(patch_path: &Path, task_id: &str) {
    let consumed = patch_path.with_file_name("state.patch.applied.json");
    // Fast path: atomic rename on the same filesystem.
    let rename_err = match std::fs::rename(patch_path, &consumed) {
        Ok(()) => return,
        Err(e) => e,
    };

    // Only a cross-filesystem rename (EXDEV) — which hits when the package
    // sits on disk while runtime/outputs is bind-mounted from tmpfs — warrants
    // the copy+remove fallback (mirrors `maybe_quarantine_malformed`). Any
    // other rename failure is logged and left best-effort.
    if rename_err.raw_os_error() != Some(libc::EXDEV) {
        tracing::error!(
            target: "patch",
            task_id = %task_id,
            consumed = %consumed.display(),
            error = %rename_err,
            "merged but rename failed"
        );
        return;
    }

    tracing::warn!(
        task_id = task_id,
        patch_path = %patch_path.display(),
        consumed_path = %consumed.display(),
        rename_error = %rename_err,
        "[patch] rename across filesystems; falling back to copy+remove"
    );
    consume_via_copy(patch_path, &consumed, task_id, &rename_err);
}

/// Copy+remove fallback for `consume_patch` on a cross-filesystem (EXDEV)
/// rename. A failed remove of the original after a successful copy is
/// best-effort (logged, non-fatal); a failed copy is logged at error.
fn consume_via_copy(
    patch_path: &Path,
    consumed: &Path,
    task_id: &str,
    rename_err: &std::io::Error,
) {
    match std::fs::copy(patch_path, consumed) {
        Ok(_) => {
            if let Err(remove_err) = std::fs::remove_file(patch_path) {
                tracing::warn!(
                    target: "patch",
                    task_id = %task_id,
                    consumed = %consumed.display(),
                    error = %remove_err,
                    "merged and copied but remove of original failed"
                );
            }
        }
        Err(copy_err) => {
            tracing::error!(
                target: "patch",
                task_id = %task_id,
                consumed = %consumed.display(),
                rename_error = %rename_err,
                copy_error = %copy_err,
                "merged but rename + copy fallback both failed"
            );
        }
    }
}

fn read_workflow(dir: &Path) -> Result<DAG> {
    // Cap WORKFLOW.json reads so a corrupted file
    // can't OOM the harness during dispatch-state recovery.
    let raw = read_capped(&dir.join("WORKFLOW.json"), resolve_max_bytes())
        .context("reading WORKFLOW.json")?;
    serde_json::from_str(&raw).context("parsing WORKFLOW.json")
}

/// Outcome of applying one patch — distinguishes a real state
/// transition (`Transition`) from the no-op heartbeat path
/// (`HeartbeatNoop`) so callers can decide whether to count the
/// iteration as productive. Both are success cases; both consume the
/// patch file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PatchOutcome {
    /// State changed. Task is now `Completed | Blocked | Failed`.
    Transition,
    /// `running → running` no-op. State unchanged; sidecar evidence and
    /// heartbeat updated.
    HeartbeatNoop,
}

fn try_apply_patch(
    dag: &mut DAG,
    task_id: &str,
    patch_path: &Path,
    expected_identity: Option<(&str, u64)>,
) -> Result<PatchOutcome> {
    // Cap per-task state.patch.json so a malicious
    // agent can't OOM the harness through this trusted-input path.
    let raw = read_capped(patch_path, resolve_max_bytes())
        .with_context(|| format!("reading {}", patch_path.display()))?;
    let patch: StatePatch =
        serde_json::from_str(&raw).with_context(|| format!("parsing {}", patch_path.display()))?;
    if let Some((expected_run_id, expected_epoch)) = expected_identity {
        if patch.harness_run_id.as_deref() != Some(expected_run_id)
            || patch.dispatch_epoch != Some(expected_epoch)
        {
            anyhow::bail!(
                "dispatch-identity-mismatch: patch has harness_run_id={:?} dispatch_epoch={:?}, expected harness_run_id={} dispatch_epoch={}",
                patch.harness_run_id,
                patch.dispatch_epoch,
                expected_run_id,
                expected_epoch
            );
        }
    }
    // Read-only validation pass — gather everything we need from `dag`
    // before taking the mutable borrow. Lets the dep-completed guard
    // below inspect peer entries in `dag.tasks` without tripping NLL.
    let task = dag
        .tasks
        .get(task_id)
        .with_context(|| format!("task {} not in DAG", task_id))?;
    if let Some(expected) = patch.from.as_deref() {
        let actual = state_tag(&task.state);
        if expected != actual {
            anyhow::bail!(
                "from-mismatch: patch expected {}, current is {}",
                expected,
                actual
            );
        }
    }
    // `running → running` is the legitimate "still in flight, sentinel
    // pending" yield — see the rustdoc on `StatePatch::to`. The live
    // state's `started_at` and `remote` are preserved verbatim so the
    // pre-marked dispatch metadata isn't clobbered by a fresher
    // timestamp the agent pulled from `chrono::Utc::now()`. Heartbeat
    // is touched and the optional `note` lands in the audit sidecar.
    if matches!(patch.to, TaskState::Running { .. }) {
        if !matches!(task.state, TaskState::Running { .. }) {
            anyhow::bail!(
                "running-noop patch rejected: live state is {} (no-op patches require live task to be already running)",
                state_tag(&task.state)
            );
        }
        write_last_agent_check_sidecar(patch_path, task_id, patch.note.as_deref());
        touch_heartbeat_for(patch_path);
        return Ok(PatchOutcome::HeartbeatNoop);
    }
    if !is_agent_target(&patch.to) {
        anyhow::bail!(
            "patch.to is {} — agents may only patch to completed/blocked/failed (or running for a no-op heartbeat patch)",
            state_tag(&patch.to)
        );
    }
    // Terminal patches require the live task to be Running. The harness
    // flips a task to Running at dispatch; a terminal patch on any
    // other live state is either stale (left over from a prior run) or
    // rogue (an agent writing a patch for a task it wasn't dispatched
    // to run). Why: under the closed agent contract, only the
    // currently-dispatched task should ever produce a transition.
    if !matches!(task.state, TaskState::Running { .. }) {
        anyhow::bail!(
            "terminal-patch rejected: live state is {}, terminal patches require running",
            state_tag(&task.state)
        );
    }
    // `Completed` terminal additionally requires every upstream dep to
    // already be `Completed`. DAG invariant — guards against any path
    // that lets a task into Running before its deps are met (rogue
    // agent direct-write to WORKFLOW.json, dispatch bug, manual edit).
    // Without this, an orphan `from: running, to: completed` patch can
    // promote a task past its unmet upstream chain — the May-2026
    // bulk-rnaseq incident.
    if matches!(patch.to, TaskState::Completed { .. }) {
        let deps = task.depends_on.clone();
        for dep_id in &deps {
            let dep = dag
                .tasks
                .get(dep_id)
                .with_context(|| format!("dep {} of {} missing from DAG", dep_id, task_id))?;
            if !matches!(dep.state, TaskState::Completed { .. }) {
                anyhow::bail!(
                    "dep-not-completed: dep {} is {} — {} cannot transition to completed until every upstream dep is completed",
                    dep_id,
                    state_tag(&dep.state),
                    task_id
                );
            }
        }
    }
    let task = dag
        .tasks
        .get_mut(task_id)
        .expect("task lookup verified above");
    task.state = patch.to;
    Ok(PatchOutcome::Transition)
}

/// Persist the agent's free-form evidence to
/// `runtime/outputs/<task_id>/last_agent_check.json`. Best-effort: a
/// failed write is logged but does not fail the patch — the heartbeat
/// is the load-bearing signal, the sidecar is purely for audit.
fn write_last_agent_check_sidecar(patch_path: &Path, task_id: &str, note: Option<&str>) {
    let Some(dir) = patch_path.parent() else {
        return;
    };
    let now = ecaa_workflow_core::time_helpers::now_rfc3339();
    let payload = serde_json::json!({
        "task_id": task_id,
        "checked_at": now,
        "note": note.unwrap_or(""),
    });
    let target = dir.join("last_agent_check.json");
    let pretty = match serde_json::to_string_pretty(&payload) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(
                target: "patch",
                task_id = %task_id,
                error = %e,
                "last_agent_check serialize failed"
            );
            return;
        }
    };
    if let Err(e) = std::fs::write(&target, pretty) {
        tracing::warn!(
            target: "patch",
            task_id = %task_id,
            target = %target.display(),
            error = %e,
            "last_agent_check write failed"
        );
    }
}

/// Mirror of `main.rs::touch_heartbeat` scoped to the patch path. Kept
/// here (rather than calling main's helper) so this module remains
/// self-contained and unit-testable. Best-effort.
fn touch_heartbeat_for(patch_path: &Path) {
    let Some(dir) = patch_path.parent() else {
        return;
    };
    let hb = dir.join(".heartbeat");
    let now = ecaa_workflow_core::time_helpers::now_rfc3339();
    let _ = std::fs::write(&hb, now);
}

fn state_tag(state: &TaskState) -> &'static str {
    match state {
        TaskState::Pending => "pending",
        TaskState::Ready => "ready",
        TaskState::Running { .. } => "running",
        TaskState::Completed { .. } => "completed",
        TaskState::Failed { .. } => "failed",
        TaskState::Blocked { .. } => "blocked",
    }
}

/// Terminal-ish targets a patch may directly write to a task's state.
/// `Running` is special-cased upstream as a no-op heartbeat patch.
fn is_agent_target(state: &TaskState) -> bool {
    matches!(
        state,
        TaskState::Completed { .. } | TaskState::Failed { .. } | TaskState::Blocked { .. }
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use ecaa_workflow_core::dag::{Assignee, BlockedRecord, ResourceClass, Task, TaskKind};
    use std::collections::BTreeMap;

    fn fixture_dag() -> DAG {
        let mut t = BTreeMap::new();
        t.insert(
            "compute".into(),
            Task {
                kind: TaskKind::Computation,
                state: TaskState::Running {
                    started_at: "2026-01-01T00:00:00Z".into(),
                    remote: None,
                },
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
        DAG {
            version: "1".into(),
            schema_version: ecaa_workflow_core::dag::current_dag_schema_version(),
            workflow_id: "w".into(),
            current_task: None,
            tasks: t,
            reverse_deps: BTreeMap::new(),
            run_id: None,
        }
    }

    fn write_workflow(dir: &Path, dag: &DAG) {
        std::fs::write(
            dir.join("WORKFLOW.json"),
            serde_json::to_string_pretty(dag).unwrap(),
        )
        .unwrap();
    }

    fn write_patch(dir: &Path, task_id: &str, patch: &serde_json::Value) {
        let outputs = dir.join("runtime/outputs").join(task_id);
        std::fs::create_dir_all(&outputs).unwrap();
        std::fs::write(
            outputs.join("state.patch.json"),
            serde_json::to_string_pretty(patch).unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn applies_completed_patch_and_consumes_file() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        write_workflow(dir, &fixture_dag());
        write_patch(
            dir,
            "compute",
            &serde_json::json!({
                "from": "running",
                "to": { "status": "completed", "result": { "n_rows": 42 } }
            }),
        );
        let merged = apply_pending_patches(dir, &[TaskId::from("compute")]).unwrap();
        let task = merged.tasks.get("compute").unwrap();
        match &task.state {
            TaskState::Completed { result } => {
                assert_eq!(result["n_rows"], 42);
            }
            other => panic!("expected Completed, got {:?}", other),
        }
        // Patch file consumed → renamed to.applied.
        assert!(!dir
            .join("runtime/outputs/compute/state.patch.json")
            .exists());
        assert!(dir
            .join("runtime/outputs/compute/state.patch.applied.json")
            .exists());
    }

    #[test]
    fn strict_dispatch_accepts_matching_identity() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        write_workflow(dir, &fixture_dag());
        write_patch(
            dir,
            "compute",
            &serde_json::json!({
                "from": "running",
                "harness_run_id": "run-1",
                "dispatch_epoch": 7,
                "to": { "status": "completed", "result": { "strict": true } }
            }),
        );
        let merged = apply_pending_patches_strict(
            dir,
            &[PickedDispatch {
                task_id: "compute".into(),
                harness_run_id: "run-1".into(),
                epoch: 7,
            }],
        )
        .unwrap();
        match &merged.tasks.get("compute").unwrap().state {
            TaskState::Completed { result } => assert_eq!(result["strict"], true),
            other => panic!("expected Completed, got {:?}", other),
        }
        assert!(!dir
            .join("runtime/outputs/compute/state.patch.json")
            .exists());
        assert!(dir
            .join("runtime/outputs/compute/state.patch.applied.json")
            .exists());
    }

    #[test]
    fn strict_dispatch_ignores_orphan_patch() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        write_workflow(dir, &fixture_dag());
        write_patch(
            dir,
            "compute",
            &serde_json::json!({
                "from": "running",
                "harness_run_id": "run-1",
                "dispatch_epoch": 7,
                "to": { "status": "completed", "result": { "orphan": true } }
            }),
        );
        let merged = apply_pending_patches_strict(dir, &[]).unwrap();
        assert!(matches!(
            merged.tasks.get("compute").unwrap().state,
            TaskState::Running { .. }
        ));
        assert!(dir
            .join("runtime/outputs/compute/state.patch.json")
            .exists());
    }

    #[test]
    fn strict_dispatch_rejects_mismatched_identity() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        write_workflow(dir, &fixture_dag());
        write_patch(
            dir,
            "compute",
            &serde_json::json!({
                "from": "running",
                "harness_run_id": "run-1",
                "dispatch_epoch": 8,
                "to": { "status": "completed", "result": { "wrong_epoch": true } }
            }),
        );
        let merged = apply_pending_patches_strict(
            dir,
            &[PickedDispatch {
                task_id: "compute".into(),
                harness_run_id: "run-1".into(),
                epoch: 7,
            }],
        )
        .unwrap();
        assert!(matches!(
            merged.tasks.get("compute").unwrap().state,
            TaskState::Running { .. }
        ));
        assert!(dir
            .join("runtime/outputs/compute/state.patch.json")
            .exists());
    }

    #[test]
    fn applies_blocked_patch_with_record() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        write_workflow(dir, &fixture_dag());
        write_patch(
            dir,
            "compute",
            &serde_json::json!({
                "from": "running",
                "to": {
                    "status": "blocked",
                    "record": {
                        "reason": "needs sme",
                        "attempts": []
                    }
                }
            }),
        );
        let merged = apply_pending_patches(dir, &[TaskId::from("compute")]).unwrap();
        match &merged.tasks.get("compute").unwrap().state {
            TaskState::Blocked { record } => assert_eq!(record.reason, "needs sme"),
            other => panic!("expected Blocked, got {:?}", other),
        }
    }

    #[test]
    fn missing_patch_falls_back_to_workflow_json() {
        // Legacy agent: wrote WORKFLOW.json directly, no patch file.
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let mut dag = fixture_dag();
        dag.tasks.get_mut("compute").unwrap().state = TaskState::Completed {
            result: serde_json::json!({"legacy": true}),
        };
        write_workflow(dir, &dag);
        let merged = apply_pending_patches(dir, &[TaskId::from("compute")]).unwrap();
        match &merged.tasks.get("compute").unwrap().state {
            TaskState::Completed { result } => assert_eq!(result["legacy"], true),
            other => panic!(
                "expected Completed (from legacy WORKFLOW.json), got {:?}",
                other
            ),
        }
    }

    #[test]
    fn from_mismatch_is_skipped_and_workflow_state_kept() {
        // Patch claims `from: ready` but live state is `running` →
        // refuse to apply. Stale patch from a prior run can't clobber.
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        write_workflow(dir, &fixture_dag());
        write_patch(
            dir,
            "compute",
            &serde_json::json!({
                "from": "ready",
                "to": { "status": "completed", "result": {} }
            }),
        );
        let merged = apply_pending_patches(dir, &[TaskId::from("compute")]).unwrap();
        match &merged.tasks.get("compute").unwrap().state {
            TaskState::Running { .. } => {}
            other => panic!("expected state preserved as Running, got {:?}", other),
        }
        // Skipped patch stays on disk for debugging — caller did not
        // consume it.
        assert!(dir
            .join("runtime/outputs/compute/state.patch.json")
            .exists());
    }

    #[test]
    fn running_running_noop_preserves_state_and_writes_evidence() {
        // The legitimate "still in flight, sentinel pending" yield.
        // Live state is Running; agent confirms forward progress and
        // writes a no-op patch carrying free-form evidence. Patch is
        // accepted, state's `started_at` is preserved verbatim, and
        // `last_agent_check.json` + `.heartbeat` are written next to
        // the consumed patch.
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let mut dag = fixture_dag();
        // Pin a known started_at so we can assert it's preserved.
        dag.tasks.get_mut("compute").unwrap().state = TaskState::Running {
            started_at: "2026-01-01T00:00:00Z".into(),
            remote: None,
        };
        write_workflow(dir, &dag);
        write_patch(
            dir,
            "compute",
            &serde_json::json!({
                "from": "running",
                "to": { "status": "running", "started_at": "2099-12-31T23:59:59Z" },
                "note": "Rscript pid 42 healthy at 99.7% CPU; sentinel not yet emitted",
            }),
        );
        let merged = apply_pending_patches(dir, &[TaskId::from("compute")]).unwrap();
        match &merged.tasks.get("compute").unwrap().state {
            TaskState::Running { started_at, .. } => {
                // Critical: the patch's invented started_at MUST be
                // discarded. The pre-marked dispatch metadata stays.
                assert_eq!(started_at, "2026-01-01T00:00:00Z");
            }
            other => panic!("expected state preserved as Running, got {:?}", other),
        }
        // Patch consumed.
        assert!(!dir
            .join("runtime/outputs/compute/state.patch.json")
            .exists());
        assert!(dir
            .join("runtime/outputs/compute/state.patch.applied.json")
            .exists());
        // Sidecar evidence written.
        let sidecar =
            std::fs::read_to_string(dir.join("runtime/outputs/compute/last_agent_check.json"))
                .expect("last_agent_check.json must be written");
        let payload: serde_json::Value = serde_json::from_str(&sidecar).unwrap();
        assert_eq!(payload["task_id"], "compute");
        assert!(payload["note"].as_str().unwrap().contains("Rscript pid 42"));
        assert!(!payload["checked_at"].as_str().unwrap().is_empty());
        // Heartbeat refreshed.
        assert!(dir.join("runtime/outputs/compute/.heartbeat").exists());
    }

    #[test]
    fn running_running_rejected_when_live_state_not_running() {
        // Defense in depth: a stale agent that wakes up and writes a
        // running-noop patch on a task the harness has since flipped
        // to Completed (e.g. via the silent-completion guard) MUST
        // NOT silently succeed. The from-mismatch guard catches the
        // primary case; this test covers the case where `from` is
        // missing but `to` is Running on a non-Running task.
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let mut dag = fixture_dag();
        dag.tasks.get_mut("compute").unwrap().state = TaskState::Completed {
            result: serde_json::json!({"already_done": true}),
        };
        write_workflow(dir, &dag);
        // No "from" field — relies on the `to: Running` guard catching
        // the invalid live state.
        write_patch(
            dir,
            "compute",
            &serde_json::json!({
                "to": { "status": "running", "started_at": "2026-01-01T00:00:00Z" },
                "note": "stale check",
            }),
        );
        let merged = apply_pending_patches(dir, &[TaskId::from("compute")]).unwrap();
        match &merged.tasks.get("compute").unwrap().state {
            TaskState::Completed { result } => assert_eq!(result["already_done"], true),
            other => panic!("expected live Completed preserved, got {:?}", other),
        }
        // Rejected patch stays on disk; sidecar NOT written.
        assert!(dir
            .join("runtime/outputs/compute/state.patch.json")
            .exists());
        assert!(!dir
            .join("runtime/outputs/compute/last_agent_check.json")
            .exists());
    }

    #[test]
    fn running_running_noop_without_note_still_writes_sidecar() {
        // The `note` field is optional. A patch without one still
        // writes the sidecar (with empty note) so audit shows the
        // agent ran a check, just didn't record a rationale.
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        write_workflow(dir, &fixture_dag());
        write_patch(
            dir,
            "compute",
            &serde_json::json!({
                "from": "running",
                "to": { "status": "running", "started_at": "2026-01-01T00:00:00Z" }
            }),
        );
        let merged = apply_pending_patches(dir, &[TaskId::from("compute")]).unwrap();
        assert!(matches!(
            merged.tasks.get("compute").unwrap().state,
            TaskState::Running { .. }
        ));
        let sidecar =
            std::fs::read_to_string(dir.join("runtime/outputs/compute/last_agent_check.json"))
                .expect("sidecar still written when note absent");
        let payload: serde_json::Value = serde_json::from_str(&sidecar).unwrap();
        assert_eq!(payload["note"], "");
    }

    #[test]
    fn rejects_patch_for_unknown_task() {
        // Picks may include ids not present in the live DAG (rare but
        // possible with race against an amend). Skip; keep going.
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        write_workflow(dir, &fixture_dag());
        write_patch(
            dir,
            "ghost",
            &serde_json::json!({
                "from": "running",
                "to": { "status": "completed", "result": {} }
            }),
        );
        let merged =
            apply_pending_patches(dir, &[TaskId::from("compute"), TaskId::from("ghost")]).unwrap();
        // compute untouched (no patch); ghost not in DAG.
        assert_eq!(merged.tasks.len(), 1);
    }

    /// A malformed `state.patch.json` is renamed to a
    /// `.rejected-<timestamp>` sidecar and the task is transitioned to
    /// `Blocked { PatchUnparseable }`. The original path must no longer
    /// exist so subsequent iterations don't retry the bad bytes.
    #[test]
    fn malformed_patch_renames_and_blocks_task() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        write_workflow(dir, &fixture_dag());
        let outputs = dir.join("runtime/outputs/compute");
        std::fs::create_dir_all(&outputs).unwrap();
        let patch_path = outputs.join("state.patch.json");
        std::fs::write(&patch_path, "{ this is not valid json").unwrap();

        let merged = apply_pending_patches(dir, &[TaskId::from("compute")]).unwrap();

        // Original patch file must be gone.
        assert!(
            !patch_path.exists(),
            "malformed patch must be renamed away from state.patch.json"
        );

        // A .rejected-* sidecar must exist in the same directory.
        let rejected: Vec<_> = std::fs::read_dir(&outputs)
            .unwrap()
            .flatten()
            .filter(|e| {
                e.file_name()
                    .to_string_lossy()
                    .starts_with("state.patch.json.rejected-")
            })
            .collect();
        assert_eq!(
            rejected.len(),
            1,
            "expected exactly one .rejected-* sidecar, found: {:?}",
            rejected.iter().map(|e| e.file_name()).collect::<Vec<_>>()
        );

        // Task must be Blocked with a reason containing PatchUnparseable info.
        match &merged.tasks.get("compute").unwrap().state {
            TaskState::Blocked { record } => {
                assert!(
                    record.reason.contains("patch_unparseable"),
                    "expected PatchUnparseable kind marker in reason, got: {}",
                    record.reason
                );
            }
            other => panic!(
                "expected task Blocked after malformed patch, got {:?}",
                other
            ),
        }
    }

    /// Sanity check: a well-formed patch still applies normally after the
    /// quarantine logic was added.
    #[test]
    fn valid_patch_still_applies_normally() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        write_workflow(dir, &fixture_dag());
        write_patch(
            dir,
            "compute",
            &serde_json::json!({
                "from": "running",
                "to": { "status": "completed", "result": { "n_rows": 99 } }
            }),
        );
        let merged = apply_pending_patches(dir, &[TaskId::from("compute")]).unwrap();
        match &merged.tasks.get("compute").unwrap().state {
            TaskState::Completed { result } => {
                assert_eq!(result["n_rows"], 99);
            }
            other => panic!("expected Completed, got {:?}", other),
        }
        // Consumed normally.
        assert!(!dir
            .join("runtime/outputs/compute/state.patch.json")
            .exists());
        assert!(dir
            .join("runtime/outputs/compute/state.patch.applied.json")
            .exists());
    }

    #[test]
    fn parallel_picks_each_get_their_own_patch() {
        let mut dag = fixture_dag();
        dag.tasks.insert(
            "compute_b".into(),
            Task {
                kind: TaskKind::Computation,
                state: TaskState::Running {
                    started_at: "2026-01-01T00:00:00Z".into(),
                    remote: None,
                },
                depends_on: vec![],
                assignee: Assignee::Agent,
                description: "y".into(),
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
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        write_workflow(dir, &dag);
        write_patch(
            dir,
            "compute",
            &serde_json::json!({
                "from": "running",
                "to": { "status": "completed", "result": {"id": "a"} }
            }),
        );
        write_patch(
            dir,
            "compute_b",
            &serde_json::json!({
                "from": "running",
                "to": {
                    "status": "blocked",
                    "record": { "reason": "b waits for sme", "attempts": [] }
                }
            }),
        );
        let merged =
            apply_pending_patches(dir, &[TaskId::from("compute"), TaskId::from("compute_b")])
                .unwrap();
        assert!(matches!(
            merged.tasks.get("compute").unwrap().state,
            TaskState::Completed { .. }
        ));
        assert!(matches!(
            merged.tasks.get("compute_b").unwrap().state,
            TaskState::Blocked { .. }
        ));
        // Suppress unused-import warning from BlockedRecord above.
        let _: BlockedRecord = BlockedRecord {
            reason: "x".into(),
            attempts: vec![],
        };
    }

    /// Regression: state.patch.json on disk for a task that ISN'T in
    /// the current picks (e.g. orphaned by a prior harness binary that
    /// didn't honor the patch protocol). The orphan-scan pass should
    /// pick it up and merge when the patch's from-state still matches
    /// the live task state.
    #[test]
    fn applies_orphan_patch_not_in_picks() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        write_workflow(dir, &fixture_dag());
        write_patch(
            dir,
            "compute",
            &serde_json::json!({
                "from": "running",
                "to": { "status": "blocked", "record": { "reason": "orphan", "attempts": [] } }
            }),
        );
        // Picks is EMPTY — simulating a fresh harness startup where
        // the running task was already pre-marked by the prior run.
        let merged = apply_pending_patches(dir, &[]).unwrap();
        match &merged.tasks.get("compute").unwrap().state {
            TaskState::Blocked { record } => assert_eq!(record.reason, "orphan"),
            other => panic!("expected Blocked from orphan merge, got {:?}", other),
        }
        // Patch file consumed (renamed to.applied.json).
        assert!(!dir
            .join("runtime/outputs/compute/state.patch.json")
            .exists());
        assert!(dir
            .join("runtime/outputs/compute/state.patch.applied.json")
            .exists());
    }

    /// Orphan whose `from` no longer matches the live state must NOT
    /// be applied — protects against a prior crashed transition
    /// resurrecting after the SME has since unblocked / re-dispatched.
    #[test]
    fn orphan_patch_with_stale_from_is_skipped() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        // Live state: Completed (e.g. SME already unblocked + the task
        // re-ran successfully). Orphan patch claims from=running.
        let mut dag = fixture_dag();
        dag.tasks.get_mut("compute").unwrap().state = TaskState::Completed {
            result: serde_json::json!({"already_done": true}),
        };
        write_workflow(dir, &dag);
        write_patch(
            dir,
            "compute",
            &serde_json::json!({
                "from": "running",
                "to": { "status": "blocked", "record": { "reason": "stale", "attempts": [] } }
            }),
        );
        let merged = apply_pending_patches(dir, &[]).unwrap();
        match &merged.tasks.get("compute").unwrap().state {
            TaskState::Completed { result } => assert_eq!(result["already_done"], true),
            other => panic!("expected live Completed preserved, got {:?}", other),
        }
        // Stale orphan stays on disk for operator inspection.
        assert!(dir
            .join("runtime/outputs/compute/state.patch.json")
            .exists());
    }

    /// Orphan in a directory whose name doesn't match any task in the
    /// DAG (e.g. a stale subdirectory) is silently skipped.
    #[test]
    fn orphan_for_unknown_task_silently_skipped() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        write_workflow(dir, &fixture_dag());
        write_patch(
            dir,
            "ghost_task",
            &serde_json::json!({
                "from": "running",
                "to": { "status": "completed", "result": {} }
            }),
        );
        let merged = apply_pending_patches(dir, &[]).unwrap();
        // compute untouched.
        assert!(matches!(
            merged.tasks.get("compute").unwrap().state,
            TaskState::Running { .. }
        ));
        // Orphan stays on disk (try_apply_patch failed → not consumed).
        assert!(dir
            .join("runtime/outputs/ghost_task/state.patch.json")
            .exists());
    }

    /// Build a two-task fixture: `upstream` Pending, `downstream`
    /// Running with `depends_on=[upstream]`. Mirrors the bulk-rnaseq
    /// incident shape (DE running, normalisation pending).
    fn two_task_chain() -> DAG {
        let mut t = BTreeMap::new();
        t.insert(
            "upstream".into(),
            Task {
                kind: TaskKind::Computation,
                state: TaskState::Pending,
                depends_on: vec![],
                assignee: Assignee::Agent,
                description: "u".into(),
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
        t.insert(
            "downstream".into(),
            Task {
                kind: TaskKind::Computation,
                state: TaskState::Running {
                    started_at: "2026-01-01T00:00:00Z".into(),
                    remote: None,
                },
                depends_on: vec!["upstream".into()],
                assignee: Assignee::Agent,
                description: "d".into(),
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
        DAG {
            version: "1".into(),
            schema_version: ecaa_workflow_core::dag::current_dag_schema_version(),
            workflow_id: "w".into(),
            current_task: None,
            tasks: t,
            reverse_deps: BTreeMap::new(),
            run_id: None,
        }
    }

    /// Regression for the May-2026 bulk-rnaseq incident: an orphan
    /// `from: running, to: completed` patch on `downstream` must be
    /// rejected when `upstream` is still Pending. Without the
    /// dep-completed guard, the orphan-scan pass merged stale
    /// agent-written patches (rogue or prior-run) and the DAG snapshot
    /// showed downstream tasks Completed while their upstream chain
    /// was Pending.
    #[test]
    fn completed_orphan_patch_rejected_when_dep_not_completed() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        write_workflow(dir, &two_task_chain());
        write_patch(
            dir,
            "downstream",
            &serde_json::json!({
                "from": "running",
                "to": { "status": "completed", "result": {"spurious": true} }
            }),
        );
        let merged = apply_pending_patches(dir, &[]).unwrap();
        match &merged.tasks.get("downstream").unwrap().state {
            TaskState::Running { .. } => {}
            other => panic!(
                "expected downstream to remain Running (patch rejected by dep guard), got {:?}",
                other
            ),
        }
        assert!(matches!(
            merged.tasks.get("upstream").unwrap().state,
            TaskState::Pending
        ));
        // Rejected patch stays on disk for operator inspection (matches
        // the existing `from-mismatch` skip behavior).
        assert!(dir
            .join("runtime/outputs/downstream/state.patch.json")
            .exists());
    }

    /// Same shape as above but via the picks pass — defense in depth
    /// for the case where the harness mistakenly dispatches a task
    /// whose deps aren't met.
    #[test]
    fn completed_pick_patch_rejected_when_dep_not_completed() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        write_workflow(dir, &two_task_chain());
        write_patch(
            dir,
            "downstream",
            &serde_json::json!({
                "from": "running",
                "to": { "status": "completed", "result": {} }
            }),
        );
        let merged = apply_pending_patches(dir, &[TaskId::from("downstream")]).unwrap();
        assert!(matches!(
            merged.tasks.get("downstream").unwrap().state,
            TaskState::Running { .. }
        ));
    }

    /// Once the upstream completes, the same downstream patch should
    /// apply cleanly — proves the guard isn't over-rejecting.
    #[test]
    fn completed_patch_succeeds_when_all_deps_completed() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let mut dag = two_task_chain();
        dag.tasks.get_mut("upstream").unwrap().state = TaskState::Completed {
            result: serde_json::json!({"upstream_done": true}),
        };
        write_workflow(dir, &dag);
        write_patch(
            dir,
            "downstream",
            &serde_json::json!({
                "from": "running",
                "to": { "status": "completed", "result": {"ok": true} }
            }),
        );
        let merged = apply_pending_patches(dir, &[TaskId::from("downstream")]).unwrap();
        match &merged.tasks.get("downstream").unwrap().state {
            TaskState::Completed { result } => assert_eq!(result["ok"], true),
            other => panic!("expected downstream Completed, got {:?}", other),
        }
    }

    /// Terminal patches require the live task to be Running. A patch
    /// with `to: completed` on a Pending task (e.g. agent wrote a
    /// patch for a task it wasn't dispatched to run) must be rejected
    /// even when the `from` field is absent. Catches the rogue-agent
    /// pattern where the agent walks the output tree and stamps every
    /// directory with a completion patch.
    #[test]
    fn terminal_patch_rejected_when_live_state_not_running() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let mut dag = fixture_dag();
        dag.tasks.get_mut("compute").unwrap().state = TaskState::Pending;
        write_workflow(dir, &dag);
        // No `from` field — relies on the live-state guard.
        write_patch(
            dir,
            "compute",
            &serde_json::json!({
                "to": { "status": "completed", "result": {"rogue": true} }
            }),
        );
        let merged = apply_pending_patches(dir, &[]).unwrap();
        assert!(matches!(
            merged.tasks.get("compute").unwrap().state,
            TaskState::Pending
        ));
        assert!(dir
            .join("runtime/outputs/compute/state.patch.json")
            .exists());
    }

    #[test]
    fn state_patch_round_trips_schema_version() {
        let patch = StatePatch {
            schema_version: state_patch_schema_version(),
            from: Some("running".into()),
            harness_run_id: None,
            dispatch_epoch: None,
            to: TaskState::Completed {
                result: serde_json::json!({"ok": true}),
            },
            note: None,
        };
        let json = serde_json::to_string(&patch).unwrap();
        let back: StatePatch = serde_json::from_str(&json).unwrap();
        assert_eq!(patch.schema_version, back.schema_version);
    }

    #[test]
    fn legacy_state_patch_without_schema_version_loads_with_default() {
        let legacy = r#"{
            "to": { "status": "completed", "result": {} }
        }"#;
        let patch: StatePatch = serde_json::from_str(legacy).expect("legacy StatePatch parses");
        assert_eq!(
            patch.schema_version,
            semver::Version::new(0, 1, 0),
            "missing schema_version must default to 0.1.0"
        );
    }

    /// When picks already covered a task, the orphan-scan pass must
    /// NOT re-process it (the patch file no longer exists post-rename,
    /// but defense-in-depth: applied set guards against double-apply).
    #[test]
    fn picks_pass_takes_precedence_over_orphan_scan() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        write_workflow(dir, &fixture_dag());
        write_patch(
            dir,
            "compute",
            &serde_json::json!({
                "from": "running",
                "to": { "status": "completed", "result": {"applied_via": "picks"} }
            }),
        );
        let merged = apply_pending_patches(dir, &[TaskId::from("compute")]).unwrap();
        match &merged.tasks.get("compute").unwrap().state {
            TaskState::Completed { result } => assert_eq!(result["applied_via"], "picks"),
            other => panic!("expected picks-pass merge, got {:?}", other),
        }
        // Pure post-condition: only one.applied file, no orphan
        // residue.
        assert!(dir
            .join("runtime/outputs/compute/state.patch.applied.json")
            .exists());
        assert!(!dir
            .join("runtime/outputs/compute/state.patch.json")
            .exists());
    }

    // Characterization: a genuine parse failure quarantines the file
    // (rename to `state.patch.json.rejected-<ts>`) and returns the new path.
    #[test]
    fn quarantine_renames_on_parse_error() {
        let tmp = tempfile::tempdir().unwrap();
        let outputs = tmp.path().join("runtime/outputs/compute");
        std::fs::create_dir_all(&outputs).unwrap();
        let patch_path = outputs.join("state.patch.json");
        std::fs::write(&patch_path, b"{ not valid json").unwrap();
        // `is_parse_error` matches on the `parsing <path>` context layer.
        let err =
            anyhow::anyhow!("expected value").context(format!("parsing {}", patch_path.display()));
        let moved = maybe_quarantine_malformed(&patch_path, "compute", &err)
            .expect("parse error should quarantine and return a path");
        assert!(
            moved.exists(),
            "quarantined file should exist at returned path"
        );
        assert!(!patch_path.exists(), "original patch should be moved away");
        assert!(
            moved
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("state.patch.json.rejected-"),
            "quarantined name should be rejected-*, got {}",
            moved.display()
        );
    }

    // Characterization: a logical rejection (not a parse failure) leaves the
    // file in place for debugging and returns `None` (no quarantine).
    #[test]
    fn quarantine_skips_non_parse_error() {
        let tmp = tempfile::tempdir().unwrap();
        let outputs = tmp.path().join("runtime/outputs/compute");
        std::fs::create_dir_all(&outputs).unwrap();
        let patch_path = outputs.join("state.patch.json");
        std::fs::write(&patch_path, b"{}").unwrap();
        let err = anyhow::anyhow!("dependency not completed");
        let moved = maybe_quarantine_malformed(&patch_path, "compute", &err);
        assert!(moved.is_none(), "non-parse error must not quarantine");
        assert!(
            patch_path.exists(),
            "original patch must stay on disk for debugging"
        );
    }

    /// The `to` field is a fully-tagged `TaskState`; a `completed`
    /// transition REQUIRES the nested `result`. A bare
    /// `{"status":"completed"}` (the shape the early reconstructed agent
    /// prompt emitted) must fail to deserialize — that's the
    /// `patch_unparseable` blocker the harness raised. This pins the
    /// contract the shared agent prompt documents.
    #[test]
    fn statepatch_completed_requires_nested_result() {
        let bare = r#"{"from":"running","to":{"status":"completed"},"harness_run_id":"r","dispatch_epoch":1}"#;
        assert!(
            serde_json::from_str::<StatePatch>(bare).is_err(),
            "bare completed status without `result` must be rejected"
        );

        let canonical = r#"{"from":"running","to":{"status":"completed","result":{"summary":"ok","figures":["figures/x.png"]}},"harness_run_id":"r","dispatch_epoch":1}"#;
        let patch: StatePatch =
            serde_json::from_str(canonical).expect("canonical completed patch must parse");
        assert!(matches!(patch.to, TaskState::Completed { .. }));

        let blocked = r#"{"from":"running","to":{"status":"blocked","record":{"reason":"missing input","attempts":[]}},"harness_run_id":"r","dispatch_epoch":1}"#;
        let patch: StatePatch =
            serde_json::from_str(blocked).expect("canonical blocked patch must parse");
        assert!(matches!(patch.to, TaskState::Blocked { .. }));

        let failed = r#"{"from":"running","to":{"status":"failed","reason":"boom"},"harness_run_id":"r","dispatch_epoch":1}"#;
        let patch: StatePatch =
            serde_json::from_str(failed).expect("canonical failed patch must parse");
        assert!(matches!(patch.to, TaskState::Failed { .. }));
    }
}
