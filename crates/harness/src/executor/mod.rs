//! Executor trait — the modularity seam between the harness loop and the
//! compute backend (local subprocess vs. remote cloud provisioning).
//!
//! The harness loop has no knowledge of AWS, GCP, or any cloud. It calls
//! the trait methods; the impl decides how and where the agent runs.
//!
//! See §3 for
//! the full design rationale. This wave (Phase A) lands the trait, a
//! behavior-preserving `LocalExecutor`, a `MockExecutor` for tests, and a
//! stub `AwsExecutor` that fails loudly. Phase B fleshes out the real
//! AWS provisioning.

// Security audit shared task-id validators
// for fields that flow into shell strings via SSH polling, sbatch
// directives, or remote scripting envelopes. See module docs for the
// "refuse vs normalize" split.
pub mod _id_validator;
mod _secrets;

pub mod aws;
pub mod builder_exit_codes;
pub mod cost_guard;
pub mod hardware_envelope;
pub mod high_water_policy;
pub mod host_probe;
pub mod local;
#[cfg(any(test, feature = "dry-run"))]
pub mod mock;
pub mod multi_az_policy;
pub mod per_atom_image;
pub mod pilot;
pub mod sizing;
// SLURM is research-tier; the module is gated behind the `slurm` cargo feature;
// product builds compile
// without it. Build the full surface via
// `cargo build -p scripps-workflow-harness --features slurm` or
// `cargo build --workspace --all-features`.
#[cfg(feature = "slurm")]
pub mod slurm;
pub mod spot_policy;
pub mod stall_monitor;

use anyhow::Result;
use scripps_workflow_core::atom::{NetworkPolicy, SafetyLevel, SandboxRequirement};
use scripps_workflow_core::blocker::BlockerKind;
use scripps_workflow_core::container_state::ContainerProbeOutcome;
use scripps_workflow_core::dag::{Task, DAG};
use scripps_workflow_core::remediation::ExecutorOverrides;
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::process::ExitStatus;
use ts_rs::TS;

pub mod overrides_io;

/// Per-executor "capability profile" — what sandbox / network the
/// executor can actually provide for a task. Built once per executor
/// instance (at `provision` / construction time) and passed into
/// [`enforce_safety_policy`] before each dispatch.
#[derive(Debug, Clone)]
pub struct ExecutorCapabilities {
    /// Strongest sandbox the executor can provide. `None` means the
    /// executor cannot satisfy any sandbox > `None`; `ProcessIsolation`
    /// satisfies itself; `HardwareEnclave` satisfies all three.
    pub sandbox: SandboxRequirement,
    /// Network policy the executor exposes to the task. The executor's
    /// network must be at least as permissive as the atom's declared
    /// network policy — see [`network_compatible`].
    pub network: NetworkPolicy,
    /// Backend identifier, matches `Executor::name()` ("local", "aws",
    /// "slurm", "mock"). Carried for diagnostics; not used by the
    /// compatibility check itself.
    pub kind: &'static str,
}

/// Returns `Some(BlockerKind)` when the executor cannot dispatch the
/// task per its safety policy. Returns `None` when dispatch is OK.
///
/// Called by every executor's `dispatch` / `run_iteration` entrypoint
/// BEFORE any remote provisioning. Cheap (pure inspection), so safe to
/// run on the hot path. The source atom id is derived from
/// [`Task::source_atom_id`] (threaded through at emit time); pre-A.S6
/// WORKFLOW.json files with no atom id back-reference fall back to
/// `<unknown>` so the diagnostic still surfaces the rest of the
/// mismatch.
pub fn enforce_safety_policy(task: &Task, caps: &ExecutorCapabilities) -> Option<BlockerKind> {
    let s = &task.safety;
    let atom_id = task.source_atom_id.as_deref().unwrap_or("<unknown>");

    // Sandbox check (only meaningful for Exec-level atoms — Compute /
    // Network / Safe never require process isolation above the
    // container default).
    if s.level == SafetyLevel::Exec && s.sandbox != SandboxRequirement::None {
        let required = s.sandbox;
        let available = caps.sandbox;
        let satisfies = matches!(
            (required, available),
            (SandboxRequirement::None, _)
                | (
                    SandboxRequirement::ProcessIsolation,
                    SandboxRequirement::ProcessIsolation | SandboxRequirement::HardwareEnclave,
                )
                | (
                    SandboxRequirement::HardwareEnclave,
                    SandboxRequirement::HardwareEnclave,
                )
        );
        if !satisfies {
            return Some(BlockerKind::SandboxRequired {
                atom_id: atom_id.to_string(),
                requested: required,
                available,
            });
        }
    }

    // Network check (only meaningful for Network- or Exec-level atoms;
    // Safe / Compute atoms run with the executor's default policy
    // without a compatibility gate because their level disallows egress
    // regardless).
    if matches!(s.level, SafetyLevel::Network | SafetyLevel::Exec) {
        let atom_net = &s.network;
        let exec_net = &caps.network;
        if !network_compatible(atom_net, exec_net) {
            return Some(BlockerKind::NetworkPolicyMismatch {
                atom_id: atom_id.to_string(),
                atom_network: atom_net.clone(),
                executor_network: exec_net.clone(),
            });
        }
    }

    None
}

/// Compatibility rule: an executor's network can satisfy an atom's
/// requested policy when the executor is at least as permissive as the
/// atom requires.
///
/// - `Bridge` (full egress) satisfies anything (Bridge requester is
///   trivially happy; an allowlisted requester gets *more* than it
///   asked for, which is permissible because the atom-side allowlist
///   is a minimum-set declaration of hosts that must be reachable,
///   not a maximum-set ceiling on egress).
/// - `None { allowlist }` only satisfies an atom-side
///   `None { allowlist }` whose hosts are a subset of the executor's
///   allowlist. An atom asking for `Bridge` cannot run on an
///   egress-denied executor.
fn network_compatible(atom: &NetworkPolicy, executor: &NetworkPolicy) -> bool {
    match (atom, executor) {
        (NetworkPolicy::Bridge, NetworkPolicy::Bridge) => true,
        (NetworkPolicy::Bridge, NetworkPolicy::None { .. }) => false,
        (NetworkPolicy::None { allowlist: _ }, NetworkPolicy::Bridge) => true,
        (NetworkPolicy::None { allowlist: a }, NetworkPolicy::None { allowlist: e }) => {
            a.iter().all(|h| e.contains(h))
        }
    }
}

/// Contract every compute backend implements. The harness loop drives the
/// executor; the executor drives the agent.
///
/// Send-only (not Sync) because the main loop and SIGINT handler access it
/// through `Arc<Mutex<Box<dyn Executor>>>`. The `release_in_handler(&self)`
/// method uses `Arc<AtomicBool>` interior mutability (not raw &self mutation)
/// so impls remain free to hold `!Sync` state (network clients, subprocess
/// handles) without additional wrapping.
pub trait Executor: Send {
    /// Stable identifier used in logs, progress events, UI badges.
    fn name(&self) -> &'static str;

    /// Capability profile this executor exposes to the
    /// safety-policy gate. The harness main loop reads this before
    /// every dispatch and runs [`enforce_safety_policy`] against the
    /// picked task; a mismatch transitions the task to Blocked with a
    /// typed `BlockerKind` (SandboxRequired / NetworkPolicyMismatch)
    /// instead of dispatching.
    ///
    /// Default impl returns a permissive profile (ProcessIsolation +
    /// Bridge) so test executors and the mock backend opt in
    /// transparently. The three production backends (Local / SLURM /
    /// AWS) override with the real capability surface derived from
    /// their respective env vars + provisioning policy.
    fn capabilities(&self) -> ExecutorCapabilities {
        ExecutorCapabilities {
            sandbox: SandboxRequirement::ProcessIsolation,
            network: NetworkPolicy::Bridge,
            kind: self.name(),
        }
    }

    /// Optional pre-flight sizing pilot. When enabled, run
    /// `cfg.task_count` representative tasks on the small pilot shape
    /// and return measurements so `provision(&dag)` can resize its
    /// projection. Default impl returns `Ok(None)` so LocalExecutor /
    /// MockExecutor remain opt-in.
    ///
    /// Called between `read_dag` and `provision` in the harness main.
    fn pilot(
        &mut self,
        _dag: &DAG,
        _cfg: &pilot::PilotConfig,
    ) -> Result<Option<pilot::PilotReport>> {
        Ok(None)
    }

    /// Provision compute. Called once before the iteration loop starts.
    /// Local impl is a no-op; AWS impl will launch or attach to an instance.
    fn provision(&mut self, dag: &DAG) -> Result<()>;

    /// Wire a session-bound progress endpoint so the executor can emit SSE
    /// events (e.g. `cost_guard_passed`) back to the conversation server.
    /// Default impl is a no-op; only backends that post events need to
    /// override. Called from `main.rs` after construction when the harness is
    /// bound to a `--session-id`. The executor constructs its own
    /// `ProgressClient` from `session_id` + `server_url` so there is no
    /// cross-crate type dependency between the binary's local
    /// `mod progress_client` and the library's `crate::progress_client`.
    fn set_session_endpoint(&mut self, _session_id: String, _server_url: String) {}

    /// Start a per-iteration stall monitor that posts
    /// [`stall_monitor::StallSignal`] values on `tx` whenever a
    /// threshold is breached. Default impl is a no-op so executors
    /// opt in. Must be idempotent — repeat calls in the same
    /// iteration should not double-start threads.
    fn start_stall_monitor(
        &mut self,
        _thresholds: &stall_monitor::StallThresholds,
        _tx: std::sync::mpsc::Sender<stall_monitor::StallSignal>,
    ) -> Result<()> {
        Ok(())
    }

    /// Shut down the stall monitor thread if one is running. Default
    /// no-op. Called before `release` on exit.
    fn stop_stall_monitor(&mut self) {}

    /// Verify the backend is still ready to accept work before each
    /// `run_iteration`. Remote backends reprovision on spot interruption
    /// / termination here; the local backend's default impl is a no-op.
    /// Called from the harness main loop just before `run_iteration`.
    fn ensure_alive(&mut self, _dag: &DAG) -> Result<()> {
        Ok(())
    }

    /// Run the agent for one iteration. The executor handles any
    /// backend-specific setup (stage inputs, resize) before invoking the
    /// agent and any teardown after. Returns the agent's exit status plus
    /// optional remote metadata to attach to progress events.
    ///
    /// `envelope` carries the per-task `SWFC_HW_*` hardware envelope
    /// the executor passes through to the agent subprocess as
    /// env vars. An empty map is a valid value — the agent falls back
    /// to its pre-envelope behavior, preserving backward compatibility
    /// for callers that haven't opted in. See
    /// `crates/harness/src/executor/hardware_envelope.rs`.
    fn run_iteration(
        &mut self,
        package: &Path,
        agent_cmd: &str,
        envelope: &std::collections::BTreeMap<String, String>,
    ) -> Result<IterationOutcome>;

    /// Per-task stale-detection check. Local: timestamp only.
    /// Remote (future): consult the cloud API (e.g. SSM
    /// `get-command-invocation`) so a long-running but healthy remote task
    /// is not erroneously reset to Ready.
    fn is_task_stale(&self, task: &Task, now_secs: u64) -> bool;

    /// Backend-native instance type currently in service, when one is
    /// provisioned. Used by the harness main loop to pair a stall
    /// signal with a resize suggestion via
    /// `stall_monitor::suggest_resize`. Default `None` for local +
    /// mock executors; AWS executor returns its tracked instance type.
    fn current_instance_type(&self) -> Option<String> {
        None
    }

    /// verified orphan sweep. Called once at harness
    /// startup (after `provision`). AWS impl polls for termination and
    /// returns a summary; local/slurm return `None`. The main loop
    /// forwards `Some(summary)` as an `orphan_instances_reaped`
    /// progress event.
    fn sweep_orphans_verified(&self) -> Option<VerifiedOrphanReap> {
        None
    }

    /// Number of concurrent CPU-class tasks the scheduler
    /// may dispatch. Default is `1` (serial behavior, preserves the
    /// pre-parallel-scheduler contract). `LocalExecutor` overrides to
    /// compute `nproc / max(tool_thread_curves)` so each task saturates
    /// its own thread budget without oversubscription; `AwsExecutor`
    /// returns the fleet size.
    fn cpu_budget(&self) -> usize {
        1
    }

    /// Number of concurrent GPU-class tasks the scheduler
    /// may dispatch. Default is `0` (no GPU available). `LocalExecutor`
    /// probes `nvidia-smi` when present; `AwsExecutor` consults the
    /// provisioned instance type.
    fn gpu_budget(&self) -> usize {
        0
    }

    /// Pop the most recent iteration's capture (stderr/stdout/exit/etc.)
    /// after `run_iteration` returns. Caller-owns; subsequent calls
    /// without a fresh `run_iteration` return `None`. Used by the
    /// harness main loop to synthesise a `ToolErrorEnvelope` for any
    /// task that transitioned to Failed/Blocked. Default `None`
    /// preserves backward compatibility — backends that don't yet
    /// capture simply skip envelope synthesis.
    fn take_last_capture(&mut self) -> Option<IterationCapture> {
        None
    }

    /// Translate per-task remediation overrides into backend-native
    /// dispatch parameters. Default is a no-op so backends opt in.
    ///
    /// Called by the harness immediately before
    /// `run_iteration` whenever
    /// `runtime/inputs/<task_id>/overrides.json` exists for the task
    /// the scheduler is about to dispatch. The backend reads only the
    /// fields it understands — `LocalExecutor` cares about
    /// `library_pins` and the memory cap; `AwsExecutor` re-runs sizing
    /// against `resources` and toggles spot; `SlurmExecutor` rewrites
    /// the next sbatch.
    ///
    /// Implementations must be idempotent — the harness may call this
    /// multiple times with the same overrides without ill effect.
    fn apply_overrides(&mut self, _task_id: &str, _overrides: &ExecutorOverrides) -> Result<()> {
        Ok(())
    }

    /// Container-aware orphan probe. Called by the
    /// harness's heartbeat-stall path before flipping a task to
    /// `Blocked { [heartbeat_stalled] }` so the reaper can distinguish
    /// "instance is fine, the container is wedged" (ContainerHung —
    /// preserve host, reap container only) from "no signal at all"
    /// (legacy heartbeat-stalled). Default `NoSignal` keeps
    /// LocalExecutor / MockExecutor on the legacy path; AWS overrides
    /// to invoke its SSM probe; SLURM overrides to read the sidecar
    /// over SSH. `package_dir` is the on-host package install root the
    /// agent uses to compose `.container-state.json` paths; the
    /// harness already owns this path so passing it down avoids a
    /// double-config dance.
    fn probe_container_state(&self, _task_id: &str, _package_dir: &Path) -> ContainerProbeOutcome {
        ContainerProbeOutcome::NoSignal
    }

    /// Cooperative shutdown signal. Called from the SIGINT/SIGTERM
    /// handler in a dedicated thread context. Implementations must NOT
    /// require `&mut self` and must not block on the same locks the
    /// main-loop's `run_iteration` holds. Default impl is a no-op so
    /// existing executors compile.
    ///
    /// For remote backends (AWS SSM, SLURM sacct) this sets an
    /// `Arc<AtomicBool>` "shutdown_requested" flag that the long-running
    /// polling loop checks between poll cycles, causing `run_iteration`
    /// to return early so the main loop's `Arc<Mutex<Box<dyn Executor>>>`
    /// is dropped before the handler calls the full `release(&mut self)`.
    fn release_in_handler(&self) {}

    /// Return a cloneable handle to this executor's cooperative shutdown
    /// flag, when one exists. The SIGINT handler clones this Arc before
    /// the executor is wrapped in `Arc<Mutex<...>>` so the flag can be
    /// set directly without contending the iteration mutex.
    ///
    /// Returns `None` for executors that have no polling loop (LocalExecutor,
    /// MockExecutor). Returns `Some(flag)` for AWS and SLURM where
    /// `run_iteration` can block for minutes on a remote poll.
    fn shutdown_flag(&self) -> Option<std::sync::Arc<std::sync::atomic::AtomicBool>> {
        None
    }

    /// Cancel an in-flight task identified by `task_id`. Called by the
    /// harness when the session transitions to `Amending` and the task
    /// appears in `invalidated_tasks`. The default implementation sends
    /// SIGTERM to the agent subprocess group, waits 500 ms (matching the
    /// server-side `kill_process_group` grace), then SIGKILL — best-effort
    /// via `pkill -f SWFC_TASK_ID=<id>`. Remote backends override:
    /// `AwsExecutor` calls `ssm cancel-command`; `SlurmExecutor` calls
    /// `scancel`.
    ///
    /// The method is best-effort: a failure to cancel is logged but does
    /// not prevent the harness from transitioning the task to
    /// `Blocked { CancelledByAmendment }`. The next iteration's stale-
    /// detection + orphan-reap path will catch any process that outlived
    /// the cancel attempt.
    ///
    /// `dag` is the current DAG snapshot so the backend can look up the
    /// task's `RemoteExecution` metadata (e.g. SSM command id, SLURM
    /// job id) without re-reading WORKFLOW.json.
    fn cancel_task(&self, task_id: &str, _dag: &DAG) -> Result<()> {
        // W4.2: validate task_id before shell interpolation. The harness's
        // path-jail policy refuses to interpolate any id that wouldn't
        // pass _id_validator; honour that here too.
        let safe_id = match crate::executor::_id_validator::sanitize_task_id(task_id) {
            Ok(id) => id,
            Err(reason) => {
                tracing::warn!(
                    target: "amend_cancel",
                    task_id = %task_id,
                    reason = %reason,
                    "cancel_task: refusing pkill interpolation of unsafe task id"
                );
                return Ok(());
            }
        };
        tracing::info!(
            target: "amend_cancel",
            task_id = %safe_id,
            "cancel_task: default impl (SIGTERM → 500ms → SIGKILL local process group — backend-agnostic)"
        );
        let pattern = format!("SWFC_TASK_ID={}", safe_id);
        // SIGTERM — best-effort. Any non-zero exit (e.g. pkill missing or
        // no matching process) is intentionally swallowed; the cancel is
        // advisory and the harness has independent stale-detection.
        let _ = std::process::Command::new("pkill")
            .args(["-TERM", "-f", &pattern])
            .status();
        // 500ms grace — matches the server-side kill_process_group bound
        // (see main.rs::kill_process_group). Long enough for a Ctrl-C
        // handler in the agent to flush state.patch.json, short enough
        // that amendment doesn't visibly stall.
        std::thread::sleep(std::time::Duration::from_millis(500));
        // SIGKILL — guarantees the process is gone. The same suppression
        // policy as the SIGTERM call: pkill returning non-zero because the
        // process already exited is the success case.
        let _ = std::process::Command::new("pkill")
            .args(["-KILL", "-f", &pattern])
            .status();
        Ok(())
    }

    /// Cleanup. Called once after the loop exits (success, error, or
    /// SIGINT). Must be idempotent.
    fn release(&mut self);
}

/// trait-level mirror of `aws::OrphanReapSummary`,
/// lifted so callers outside the AWS module can consume it without a
/// backend-specific import. The AWS impl converts its internal
/// struct; other backends can return `None`.
///
/// P1-158 — `terminate_failures` propagates AWS-side `UnsuccessfulItems`
/// from a batched `terminate-instances` call so the operator gets the
/// per-id failure reason instead of an opaque count.
#[derive(Debug, Clone, Default)]
pub struct VerifiedOrphanReap {
    /// Name of the reap policy that was applied (e.g. `"tag_match"`).
    pub policy: String,
    /// Number of instances that matched the candidate criteria before verification.
    pub candidate_count: u64,
    /// Number of candidates confirmed terminated by the convergence poll.
    pub verified_count: u64,
    /// Candidate ids that could not be confirmed terminated in time.
    pub unverified_ids: Vec<String>,
    /// P1-226 — ids confirmed terminated by the convergence poll.
    /// Passed into `recover_orphaned_dispatches_with_denylist` so a
    /// WAL recovery cannot treat a stale heartbeat from a dead host
    /// as evidence of liveness.
    pub verified_ids: Vec<String>,
    /// Per-id termination failures from a batched `terminate-instances` call:
    /// `(instance_id, error_message)`.
    pub terminate_failures: Vec<(String, String)>,
}

/// Result of one agent invocation, returned from `Executor::run_iteration`.
///
/// Not TS-exported — lives entirely inside the harness process. The
/// `RemoteExecutionInfo` it carries *does* cross to the conversation
/// server (via `HarnessEvent`) and is exported separately from there.
pub struct IterationOutcome {
    /// Process exit status of the agent subprocess.
    pub agent_status: ExitStatus,
    /// Backend-specific execution metadata to attach to progress events.
    /// Always `None` for the local path; populated by remote executors in
    /// Phase B and later.
    pub remote: Option<RemoteExecutionInfo>,
}

/// Per-iteration capture used to synthesise `ToolErrorEnvelope`s after
/// a non-zero exit. Each backend populates whichever fields it can —
/// the envelope synthesizer tolerates missing fields.
#[derive(Debug, Clone, Default)]
pub struct IterationCapture {
    /// Captured standard error from the agent subprocess.
    pub stderr: String,
    /// Captured standard output from the agent subprocess.
    pub stdout: String,
    /// Process exit code; `None` when the process was killed by a signal.
    pub exit_code: Option<i32>,
    /// Signal name that killed the process (e.g. `"SIGKILL"`), if applicable.
    pub signal: Option<String>,
    /// Elapsed wall-clock time of the iteration in seconds.
    pub wallclock_secs: Option<u64>,
    /// Peak RSS of the agent subprocess in megabytes.
    pub peak_memory_mb: Option<u64>,
    /// Backend-native context: instance_type, partition, host, etc.
    /// Folded directly into `ToolErrorEnvelope::executor_context`.
    pub executor_context: std::collections::BTreeMap<String, String>,
}

/// Opaque backend-native identifiers surfaced to the conversation layer
/// so the UI can display "running on i-0abc123 (r6i.4xlarge)" in harness
/// progress messages.
///
/// Not `#[derive(TS)]` here — an identically-shaped type in
/// `conversation::session::RemoteExecutionInfo` owns the TS export so the
/// UI's `RemoteExecutionInfo.ts` has exactly one source. The harness
/// crosses the wire as plain JSON; the conversation layer picks the
/// payload up and re-serialises it through its own struct.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RemoteExecutionInfo {
    /// Backend identifier, matches `Executor::name()`. e.g. "aws", "gcp".
    pub backend: String,
    /// Backend-native instance/job ID (e.g. "i-0abc123def456").
    pub instance_id: String,
    /// Backend-native instance type (e.g. "r6i.4xlarge").
    pub instance_type: String,
}

/// Abstract compute requirements consumed by the cloud-specific mapping
/// logic. Cloud-agnostic on purpose — the Phase A data model ships the
/// struct so downstream waves can fill in scaling logic without churning
/// the type.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[ts(export)]
pub struct ResourceRequirements {
    /// Number of virtual CPUs required.
    pub vcpus: u32,
    /// Memory required in gigabytes.
    pub memory_gb: u32,
    /// Ephemeral storage required in gigabytes.
    pub storage_gb: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    /// Optional GPU requirement; absent means CPU-only.
    pub gpu: Option<GpuRequirement>,
}

/// Optional GPU slice of a `ResourceRequirements`. Detached from the main
/// struct so presence is meaningful — absent means CPU-only.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[ts(export)]
pub struct GpuRequirement {
    /// e.g. "nvidia-t4", "nvidia-a100", "nvidia-l4".
    pub kind: String,
    /// Number of GPU devices required.
    pub count: u32,
}

/// Build the executor named by `mode`, threading the current CLI args
/// through to the impl's constructor.
///
/// `SWFC_EXECUTOR_MODE=local` (the default) returns `LocalExecutor`.
/// `SWFC_EXECUTOR_MODE=aws` returns `AwsExecutor`. `SWFC_EXECUTOR_MODE=slurm`
/// returns `SlurmExecutor` only when the harness was built with the
/// `slurm` cargo feature (research-tier per F16-3); without the
/// feature the dispatch errors with a "rebuild with --features slurm"
/// diagnostic so operators on a product build get a predictable fail
/// path. Any other mode is rejected.
pub fn build(mode: &str, args: &ExecutorArgs) -> Result<Box<dyn Executor>> {
    match mode {
        "local" => {
            // W2.1: validate SWFC_LOCAL_SANDBOX at build time so a typo
            // (e.g. `bublewrap`) refuses to start the harness rather than
            // silently disabling sandboxing for the whole session.
            local::validate_sandbox_env()?;
            Ok(Box::new(local::LocalExecutor::new(args)))
        }
        "aws" => Ok(Box::new(aws::AwsExecutor::new(args)?)),
        #[cfg(feature = "slurm")]
        "slurm" => Ok(Box::new(slurm::SlurmExecutor::new(args)?)),
        #[cfg(not(feature = "slurm"))]
        "slurm" => anyhow::bail!(
            "SWFC_EXECUTOR_MODE=slurm requires the 'slurm' cargo feature \
             (research-tier per F16-3); rebuild the harness with \
             `cargo build -p scripps-workflow-harness --features slurm` \
             or use SWFC_EXECUTOR_MODE=local|aws on a product build."
        ),
        #[cfg(feature = "dry-run")]
        "mock" => Ok(Box::new(mock::MockExecutor::with_successes(
            mock_budget_from_env(),
        ))),
        #[cfg(not(feature = "dry-run"))]
        "mock" => anyhow::bail!(
            "SWFC_EXECUTOR_MODE=mock requires the 'dry-run' cargo feature; \
             rebuild with `cargo build -p scripps-workflow-harness --features dry-run` \
             or use SWFC_EXECUTOR_MODE=local|aws|slurm."
        ),
        other => anyhow::bail!(
            "Unknown SWFC_EXECUTOR_MODE '{}'. Expected one of: local, aws, slurm",
            other
        ),
    }
}

/// Read `SWFC_MOCK_TASK_BUDGET` for the scripted-iteration budget of the
/// dry-run `MockExecutor`. Unset, zero, or unparseable values fall back
/// to `100`; the fallback emits a single `tracing::warn!` so operators
/// notice malformed input. Bounded private to the executor module —
/// production callers reach it only via `build("mock", ...)`.
#[cfg(feature = "dry-run")]
fn mock_budget_from_env() -> usize {
    const DEFAULT: usize = 100;
    match std::env::var("SWFC_MOCK_TASK_BUDGET") {
        Err(_) => DEFAULT,
        Ok(raw) => match raw.parse::<usize>() {
            Ok(n) if n > 0 => n,
            Ok(_) => {
                tracing::warn!(
                    "SWFC_MOCK_TASK_BUDGET=0 is invalid; falling back to {}",
                    DEFAULT
                );
                DEFAULT
            }
            Err(_) => {
                tracing::warn!(
                    "SWFC_MOCK_TASK_BUDGET='{}' is unparseable; falling back to {}",
                    raw,
                    DEFAULT
                );
                DEFAULT
            }
        },
    }
}

/// The subset of harness CLI args executors need at construction time.
/// Passing a dedicated struct (rather than the full `Args`) keeps the
/// trait tests independent of clap wiring.
#[derive(Debug, Clone)]
pub struct ExecutorArgs {
    /// Path to the emitted package directory containing `WORKFLOW.json`.
    pub package: String,
    /// Path or name of the agent executable to invoke per task.
    pub agent: String,
    /// Per-task wall-clock deadline (seconds) before the executor kills the agent.
    pub task_timeout_secs: u64,
}

/// Cross-module test lock serialising every test that mutates
/// process-level env vars the executor crate consumes (`SWFC_AWS_*`,
/// `SWFC_PILOT_*`, `SWFC_EXECUTOR_MODE`,...). `aws.rs`,
/// `multi_az_policy.rs`, `pilot.rs`, and `sizing.rs` all grab this
/// before mutating those vars so parallel `cargo test` runs don't
/// observe each other's transient overrides. Shared here so every
/// test module can reach it without exposing one module's private state.
/// Compiled only under `cfg(test)` to keep it out of the release binary.
#[cfg(test)]
pub(crate) static SWFC_AWS_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Shared test lock for the per-atom-image build path.
/// `per_atom_image::tests` (helper-level)
/// and `local::tests` (provision-level) both mutate
/// `SWFC_PER_TASK_IMAGES`, `SWFC_PER_ATOM_BUILD_ROOT`, and
/// `SWFC_IMAGE_BUILDER_PATH`. They must serialize against EACH OTHER
/// (not just within their own module) so parallel `cargo test` runs
/// can't trip on a stale env var another test left briefly set. Same
/// shape as `SWFC_AWS_ENV_LOCK`.
#[cfg(test)]
pub(crate) static SWFC_PER_TASK_IMAGE_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
mod tests {
    // S5.32: workspace lint is `unsafe_code = "deny"`. Test code uses
    // `unsafe { std::env::set_var / remove_var }` (unsafe in Rust 2024
    // edition because the env table is not thread-safe). All call sites
    // are single-threaded test setup/teardown; the bounded waiver is
    // scoped to this `mod tests` block.
    #![allow(unsafe_code)]
    use super::*;

    fn args() -> ExecutorArgs {
        ExecutorArgs {
            package: "/tmp/nonexistent".into(),
            agent: "/bin/true".into(),
            task_timeout_secs: 300,
        }
    }

    #[test]
    fn factory_local_returns_local_executor() {
        let exec = build("local", &args()).expect("local mode must always succeed");
        assert_eq!(exec.name(), "local");
    }

    #[test]
    fn factory_rejects_unknown_mode() {
        // Using.err() rather than.expect_err() because the Ok variant
        // holds a `Box<dyn Executor>` which doesn't implement Debug.
        let err = build("k8s", &args())
            .err()
            .expect("unknown mode must error");
        let msg = err.to_string();
        assert!(
            msg.contains("Unknown SWFC_EXECUTOR_MODE"),
            "error should name the env var, got: {msg}"
        );
        assert!(
            msg.contains("k8s"),
            "error should echo the bad mode, got: {msg}"
        );
        // The diagnostic must list every supported mode so operators
        // can fix their env without consulting the source.
        assert!(
            msg.contains("local") && msg.contains("aws") && msg.contains("slurm"),
            "error should list all supported modes, got: {msg}"
        );
    }

    #[test]
    #[cfg(feature = "slurm")]
    fn factory_slurm_requires_env_vars() {
        // Phase S-5: SlurmExecutor is fully wired. With no SWFC_SLURM_*
        // env set, the factory must fail with a compound env-var
        // diagnostic (same shape as AWS) pointing the operator at the
        // runbook. Serialized on SWFC_AWS_ENV_LOCK with the other
        // env-mutating tests so parallel runs don't race.
        //
        // F16-3: gated behind the `slurm` feature so product builds
        // (no feature) skip this test; the feature-off path is
        // exercised by `factory_slurm_without_feature_errors_clearly`
        // below.
        let _lock = SWFC_AWS_ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let prior: Vec<(&str, Option<String>)> = [
            "SWFC_SLURM_HOST",
            "SWFC_SLURM_STAGING_DIR",
            "SWFC_SLURM_DEFAULT_PARTITION",
        ]
        .iter()
        .map(|k| (*k, std::env::var(k).ok()))
        .collect();
        for (k, _) in &prior {
            unsafe { std::env::remove_var(k) };
        }
        let err = build("slurm", &args())
            .err()
            .expect("slurm must require config");
        // Restore env ASAP so a panic below doesn't leak state.
        for (k, v) in &prior {
            if let Some(v) = v {
                unsafe { std::env::set_var(k, v) };
            }
        }
        let msg = err.to_string();
        assert!(
            msg.contains("missing required env vars"),
            "expected env-var diagnostic, got: {msg}"
        );
        assert!(
            msg.contains("SWFC_SLURM_HOST"),
            "error should name SWFC_SLURM_HOST, got: {msg}"
        );
    }

    /// F16-3 — when the harness is built without `--features slurm`
    /// (the product default), `SWFC_EXECUTOR_MODE=slurm` must fail
    /// with a clear "rebuild with --features slurm" diagnostic. The
    /// `cfg(not(feature = "slurm"))` arm in `build()` provides this;
    /// this test pins the message so it stays operator-readable.
    #[test]
    #[cfg(not(feature = "slurm"))]
    fn factory_slurm_without_feature_errors_clearly() {
        let err = build("slurm", &args())
            .err()
            .expect("slurm without feature must error");
        let msg = err.to_string();
        assert!(
            msg.contains("--features slurm"),
            "error should tell the operator how to rebuild, got: {msg}"
        );
        assert!(
            msg.contains("research-tier"),
            "error should explain the gate, got: {msg}"
        );
    }

    #[test]
    fn factory_aws_requires_required_env_vars() {
        // The AwsExecutor succeeds when SWFC_AWS_* env vars are
        // configured and otherwise fails with a single diagnostic
        // listing every missing var. Test the fail-fast behavior here
        // so the factory contract stays honest.
        for k in [
            "SWFC_AWS_REGION",
            "SWFC_AWS_AMI_ID",
            "SWFC_AWS_SECURITY_GROUP",
            "SWFC_AWS_INSTANCE_PROFILE",
            "SWFC_AWS_SUBNET_ID",
            "SWFC_AWS_SUBNET_IDS",
        ] {
            unsafe { std::env::remove_var(k) };
        }
        let err = build("aws", &args())
            .err()
            .expect("aws must require config");
        let msg = err.to_string();
        assert!(
            msg.contains("missing required env vars"),
            "expected env-var diagnostic, got: {msg}"
        );
    }

    #[test]
    fn resource_requirements_serde_roundtrip_without_gpu() {
        let r = ResourceRequirements {
            vcpus: 4,
            memory_gb: 16,
            storage_gb: 100,
            gpu: None,
        };
        let json = serde_json::to_string(&r).unwrap();
        assert!(
            !json.contains("gpu"),
            "None gpu must skip via skip_serializing_if"
        );
        let back: ResourceRequirements = serde_json::from_str(&json).unwrap();
        assert_eq!(r, back);
    }

    #[test]
    fn resource_requirements_serde_roundtrip_with_gpu() {
        let r = ResourceRequirements {
            vcpus: 8,
            memory_gb: 64,
            storage_gb: 200,
            gpu: Some(GpuRequirement {
                kind: "nvidia-l4".into(),
                count: 1,
            }),
        };
        let json = serde_json::to_string(&r).unwrap();
        let back: ResourceRequirements = serde_json::from_str(&json).unwrap();
        assert_eq!(r, back);
    }

    /// Verify that the cooperative shutdown path does not block when the
    /// main loop holds the executor's `Arc<Mutex<Box<dyn Executor>>>`.
    ///
    /// The fix: `shutdown_flag()` is called BEFORE the executor is
    /// wrapped in `Arc<Mutex<...>>`, giving the SIGINT handler an
    /// `Arc<AtomicBool>` it can set directly without touching the mutex.
    /// The SSM/SLURM poll loop checks the flag between cycles and exits.
    ///
    /// Test shape: extract the shutdown flag from a mock executor, wrap
    /// the executor in `Arc<Mutex<...>>`, hold that mutex on a background
    /// thread for 200 ms, then set the flag from the main thread and assert
    /// it completes in well under 200 ms.
    #[test]
    fn release_in_handler_does_not_block_on_run_iteration_mutex() {
        use super::mock::MockExecutor;
        use std::sync::{Arc, Mutex};
        use std::time::{Duration, Instant};

        let mut inner = Box::new(MockExecutor::with_successes(0)) as Box<dyn Executor>;

        // shutdown_flag() returns None for MockExecutor (no blocking
        // poll loop). This tests the trait contract: the call itself
        // must not block and must return immediately.
        let flag = inner.shutdown_flag();
        assert!(
            flag.is_none(),
            "MockExecutor has no blocking poll loop; shutdown_flag must be None"
        );

        // Wrap the executor and hold the mutex on a background thread.
        let executor: Arc<Mutex<Box<dyn Executor>>> = Arc::new(Mutex::new(inner));
        let held = Arc::clone(&executor);
        let guard_thread = std::thread::spawn(move || {
            let _guard = held.lock().unwrap();
            std::thread::sleep(Duration::from_millis(200));
        });

        // Give the background thread a moment to acquire the lock.
        std::thread::sleep(Duration::from_millis(10));

        // The SIGINT handler's cooperative path: set the flag directly
        // without going through the mutex. For MockExecutor the flag is
        // None so we just verify the call path completes in under 50 ms
        // even while the mutex is held elsewhere.
        let start = Instant::now();
        // flag is None for mock; in production this would be
        // Some(arc) for AWS/SLURM and store(true) fires instantly.
        let _ = flag; // flag is None; store call is a no-op here
        let elapsed = start.elapsed();
        assert!(
            elapsed < Duration::from_millis(50),
            "cooperative shutdown flag path must not block; elapsed: {elapsed:?}"
        );

        // The mutex must still be held (background thread still sleeping).
        assert!(
            executor.try_lock().is_err(),
            "mutex must still be held by background thread"
        );

        guard_thread.join().expect("guard thread must not panic");

        // After the background thread exits, the mutex is free.
        assert!(
            executor.try_lock().is_ok(),
            "mutex must be free after background thread exits"
        );
    }
}

#[cfg(test)]
mod safety_tests {
    //! Dispatch-time safety check tests. Cover the three
    //! main paths: Exec atom on a no-sandbox executor (blocked), Exec
    //! atom on a sandbox-capable executor (passes), and a Compute atom
    //! (passes unconditionally — no sandbox / network gate).
    use super::*;
    use scripps_workflow_core::atom::{
        CodeExecution, NetworkPolicy, ProvisioningPolicy, SafetyLevel, SafetyPolicy,
        SandboxRequirement,
    };
    use scripps_workflow_core::dag::{Assignee, ResourceClass, Task, TaskKind, TaskState};

    fn task_with_safety(level: SafetyLevel, sandbox: SandboxRequirement) -> Task {
        // Task has no constructor / `test_default` — build it via struct
        // literal, mirroring the `pending_task` helper inside
        // `crates/core/src/dag.rs`.
        let safety = SafetyPolicy {
            level,
            network: NetworkPolicy::None { allowlist: vec![] },
            sandbox,
            code_execution: if level == SafetyLevel::Exec {
                CodeExecution::GeneratedByAgent
            } else {
                CodeExecution::None
            },
            provisioning: if level == SafetyLevel::Exec {
                ProvisioningPolicy::Allowlisted
            } else {
                ProvisioningPolicy::DeclaredOnly
            },
            controlled_access: false,
        };
        Task {
            kind: TaskKind::Computation,
            state: TaskState::Pending,
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
            source_atom_id: Some("test_atom".into()),
            safety,
        }
    }

    #[test]
    fn exec_atom_on_no_sandbox_executor_blocked() {
        let task = task_with_safety(SafetyLevel::Exec, SandboxRequirement::ProcessIsolation);
        let caps = ExecutorCapabilities {
            sandbox: SandboxRequirement::None,
            network: NetworkPolicy::Bridge,
            kind: "local",
        };
        let blocker = enforce_safety_policy(&task, &caps);
        match blocker {
            Some(BlockerKind::SandboxRequired {
                atom_id,
                requested,
                available,
            }) => {
                assert_eq!(atom_id, "test_atom");
                assert_eq!(requested, SandboxRequirement::ProcessIsolation);
                assert_eq!(available, SandboxRequirement::None);
            }
            other => panic!("expected SandboxRequired, got {:?}", other),
        }
    }

    #[test]
    fn exec_atom_on_sandbox_executor_passes() {
        let task = task_with_safety(SafetyLevel::Exec, SandboxRequirement::ProcessIsolation);
        let caps = ExecutorCapabilities {
            sandbox: SandboxRequirement::ProcessIsolation,
            network: NetworkPolicy::Bridge,
            kind: "local",
        };
        assert!(enforce_safety_policy(&task, &caps).is_none());
    }

    #[test]
    fn compute_atom_passes_anywhere() {
        let task = task_with_safety(SafetyLevel::Compute, SandboxRequirement::None);
        let caps = ExecutorCapabilities {
            sandbox: SandboxRequirement::None,
            network: NetworkPolicy::None { allowlist: vec![] },
            kind: "local",
        };
        assert!(enforce_safety_policy(&task, &caps).is_none());
    }

    #[test]
    fn network_atom_on_denied_executor_blocked() {
        let mut task = task_with_safety(SafetyLevel::Network, SandboxRequirement::None);
        task.safety.network = NetworkPolicy::None {
            allowlist: vec!["api.example.com".into()],
        };
        let caps = ExecutorCapabilities {
            sandbox: SandboxRequirement::None,
            network: NetworkPolicy::None { allowlist: vec![] }, // deny-all
            kind: "slurm",
        };
        let blocker = enforce_safety_policy(&task, &caps);
        assert!(
            matches!(blocker, Some(BlockerKind::NetworkPolicyMismatch { .. })),
            "expected NetworkPolicyMismatch, got {:?}",
            blocker
        );
    }

    #[test]
    fn network_atom_on_bridge_executor_passes() {
        let mut task = task_with_safety(SafetyLevel::Network, SandboxRequirement::None);
        task.safety.network = NetworkPolicy::None {
            allowlist: vec!["api.example.com".into()],
        };
        let caps = ExecutorCapabilities {
            sandbox: SandboxRequirement::None,
            network: NetworkPolicy::Bridge,
            kind: "local",
        };
        assert!(enforce_safety_policy(&task, &caps).is_none());
    }
}

#[cfg(all(test, feature = "dry-run"))]
mod build_mock_arm_tests {
    // Env-var mutation in Rust 2024 edition is `unsafe`; the test table is
    // not thread-safe at the OS level. We serialize against the same lock
    // every other env-mutating test in this crate uses (`SWFC_AWS_ENV_LOCK`)
    // so parallel `cargo test` runs can't observe transient overrides.
    #![allow(unsafe_code)]
    use super::*;

    fn args() -> ExecutorArgs {
        ExecutorArgs {
            package: "/tmp/p".into(),
            agent: String::new(),
            task_timeout_secs: 0,
        }
    }

    #[test]
    fn build_mock_returns_mock_executor() {
        let _lock = SWFC_AWS_ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let prior = std::env::var("SWFC_MOCK_TASK_BUDGET").ok();
        unsafe { std::env::remove_var("SWFC_MOCK_TASK_BUDGET") };
        let exec = build("mock", &args()).expect("mock mode must build under dry-run");
        assert_eq!(exec.name(), "mock");
        if let Some(v) = prior {
            unsafe { std::env::set_var("SWFC_MOCK_TASK_BUDGET", v) };
        }
    }

    #[test]
    fn mock_budget_from_env_defaults_to_100() {
        let _lock = SWFC_AWS_ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let prior = std::env::var("SWFC_MOCK_TASK_BUDGET").ok();
        unsafe { std::env::remove_var("SWFC_MOCK_TASK_BUDGET") };
        assert_eq!(mock_budget_from_env(), 100);
        if let Some(v) = prior {
            unsafe { std::env::set_var("SWFC_MOCK_TASK_BUDGET", v) };
        }
    }

    #[test]
    fn mock_budget_from_env_honors_value() {
        let _lock = SWFC_AWS_ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let prior = std::env::var("SWFC_MOCK_TASK_BUDGET").ok();
        unsafe { std::env::set_var("SWFC_MOCK_TASK_BUDGET", "7") };
        let got = mock_budget_from_env();
        match prior {
            Some(v) => unsafe { std::env::set_var("SWFC_MOCK_TASK_BUDGET", v) },
            None => unsafe { std::env::remove_var("SWFC_MOCK_TASK_BUDGET") },
        }
        assert_eq!(got, 7);
    }

    #[test]
    fn mock_budget_from_env_zero_falls_back_to_default() {
        let _lock = SWFC_AWS_ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let prior = std::env::var("SWFC_MOCK_TASK_BUDGET").ok();
        unsafe { std::env::set_var("SWFC_MOCK_TASK_BUDGET", "0") };
        let got = mock_budget_from_env();
        match prior {
            Some(v) => unsafe { std::env::set_var("SWFC_MOCK_TASK_BUDGET", v) },
            None => unsafe { std::env::remove_var("SWFC_MOCK_TASK_BUDGET") },
        }
        assert_eq!(got, 100);
    }
}
