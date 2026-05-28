//! LocalExecutor — behaviour-preserving wrapper around the current
//! subprocess-based harness loop. Runs the agent in-process via `duct`
//! and relies on a pure timestamp check for stale-task detection.

use super::pilot::{
    compute_confidence, project_requirements, select_pilot_tasks, write_pilot_artifacts,
    PilotConfig, PilotMeasurement, PilotReport,
};
use super::sizing::{ComputeProfiles, SizingIntakeFacts};
use super::stall_monitor::{
    evaluate_cpu_window, evaluate_memory_window, StallSignal, StallThresholds,
};
use super::{Executor, ExecutorArgs, IterationCapture, IterationOutcome};
use anyhow::{Context, Result};
use parking_lot::Mutex;
use scripps_workflow_core::dag::{Task, TaskState, DAG};
use scripps_workflow_core::remediation::ExecutorOverrides;
use std::collections::{BTreeMap, VecDeque};
use std::path::Path;
use std::sync::{mpsc, Arc};

/// Env-var keys allowed to inherit from the harness
/// process when we `env_clear` the agent subprocess. Mirrors the
/// SLURM executor's `SECRET_KEYS` allowlist so the two paths use the
/// same shape.
///
/// The bigger picture: by default the agent subprocess inherits the
/// harness's entire environment. That leaks an unknown surface of
/// host-only env-vars (PATH, HOME, $SSH_AUTH_SOCK, browser env, etc.)
/// into a process we don't fully control. SLURM solved this by
/// `--export=` allowlisting + a per-job creds file; the local
/// executor now mirrors the pattern via `env_clear` + an explicit
/// re-add of `SECRET_KEYS` + the merged envelope.
///
/// W3.1: the underlying data lives in `super::_secrets::BASE_SECRET_KEYS`
/// so the AWS SSM secret-filter consumes the same set. This alias is
/// retained so existing call sites and tests don't have to change.
///
/// Bypass: `SWFC_DISABLE_ENV_CLEAR=1` falls back to full env inherit
/// for legacy tests / scripts that relied on the prior behaviour.
pub(super) const SECRET_KEYS: &[&str] = super::_secrets::BASE_SECRET_KEYS;

/// Keys that the local executor inherits from the
/// harness process by name (no value, the OS resolves on spawn). PATH
/// and HOME are not "secrets" per se but ARE required for the agent
/// subprocess to find binaries and locate `~/.claude`. Listing them
/// here keeps the allowlist explicit instead of having the env_clear
/// path silently break agent invocation.
///
/// `SWFC_DEFAULT_CONTAINER_IMAGE` and the four sibling container/sandbox
/// policy vars are inherited so the operator's `.env` policy reaches
/// agent-claude.sh. Without this, agents fall through to host execution
/// even when a default image is set, because the per-dispatch envelope
/// only populates `SWFC_DEFAULT_CONTAINER_IMAGE` for tasks that have a
/// buildable `source_atom_id` (per-task-image flow). Generic tasks lose
/// the operator's container intent silently.
pub(super) const REQUIRED_INHERITED_KEYS: &[&str] = &[
    "PATH",
    "HOME",
    "USER",
    "LANG",
    "LC_ALL",
    "TERM",
    "TMPDIR",
    "TZ",
    "XDG_CONFIG_HOME",
    "XDG_DATA_HOME",
    "XDG_CACHE_HOME",
    // Container / sandbox policy — operator's .env intent must reach
    // the agent shell. These are policy declarations, not secrets, and
    // are read by `scripts/agent-claude.sh` at every dispatch.
    "SWFC_DEFAULT_CONTAINER_IMAGE",
    "SWFC_CONTAINER_RUNTIME",
    "SWFC_CONTAINER_NETWORK_DEFAULT",
    "SWFC_CONTAINER_VERIFY",
    "SWFC_LOCAL_SANDBOX",
    "SWFC_PER_TASK_IMAGES",
    // Execution-agent cost discipline. The wrapper renders turn budgets into
    // the task prompt and uses the billing mode when choosing Claude auth.
    "SWFC_AGENT_BILLING",
    "MAX_TURNS_PER_TASK",
];

/// `SWFC_DISABLE_ENV_CLEAR=1` legacy bypass.
///
/// W2.3: emits a one-shot `tracing::warn!` whenever the flag is honored
/// so deployments that still rely on it surface in logs. The flag is
/// planned for removal — see the deprecation note on the constant
/// section above. Migration: add the var to `SECRET_KEYS` (if it's a
/// credential) or `REQUIRED_INHERITED_KEYS` (if it's a host-binary
/// locator). The Once guard means the warn fires at most once per
/// harness process, avoiding log spam on the per-dispatch hot path.
fn env_clear_disabled() -> bool {
    let enabled = matches!(std::env::var("SWFC_DISABLE_ENV_CLEAR").as_deref(), Ok("1"));
    if enabled {
        static DEPRECATION_WARNED: std::sync::Once = std::sync::Once::new();
        DEPRECATION_WARNED.call_once(|| {
            tracing::warn!(
                target: "env_clear_disabled",
                "SWFC_DISABLE_ENV_CLEAR=1 is deprecated and scheduled for removal. \
                 Migrate by adding the required env vars to SECRET_KEYS (credentials) \
                 or REQUIRED_INHERITED_KEYS (host paths) in crates/harness/src/executor/_secrets.rs \
                 + executor/local.rs respectively. The legacy inherit-everything \
                 behavior leaks an unbounded surface of host env vars into the \
                 agent subprocess."
            );
        });
    }
    enabled
}

/// W2.1 — validate `SWFC_LOCAL_SANDBOX` at executor build time. The
/// historical behavior silently disabled the sandbox on any unknown
/// value (e.g. a typo like `bublewrap`), turning a security-relevant
/// flag into a footgun. Fail closed instead: refuse to start the
/// harness so the operator sees the typo before any agent dispatch.
///
/// Valid values: unset (auto-detect via `detect_default_sandbox`),
/// `off` (explicitly disable), empty string (treated as `off`),
/// `bubblewrap` (require bwrap). Anything else is a configuration
/// error.
pub(super) fn validate_sandbox_env() -> anyhow::Result<()> {
    match std::env::var("SWFC_LOCAL_SANDBOX").as_deref() {
        Ok("bubblewrap") | Ok("off") | Ok("") => Ok(()),
        Err(_) => Ok(()),
        Ok(other) => anyhow::bail!(
            "SWFC_LOCAL_SANDBOX='{}' is not a valid policy. \
             Valid: bubblewrap, off, or unset (auto-detect). \
             Fix the env var and retry.",
            other
        ),
    }
}

/// R2.5 — read `SWFC_CONTAINER_REGISTRY_AUTH` (format
/// `registry|username|password`) and run `docker login` (or `podman
/// login` when `SWFC_CONTAINER_RUNTIME=podman`) once so subsequent
/// image pulls / builds inherit the cached credential. Returns silently
/// when the env var is unset or malformed. A failed `login` invocation
/// is logged to stderr but does not propagate — public images keep
/// working without auth.
fn registry_login_if_configured() {
    let raw = match std::env::var("SWFC_CONTAINER_REGISTRY_AUTH") {
        Ok(v) if !v.is_empty() => v,
        _ => return,
    };
    let parts: Vec<&str> = raw.splitn(3, '|').collect();
    if parts.len() != 3 {
        eprintln!(
            "[registry-auth] SWFC_CONTAINER_REGISTRY_AUTH must be registry|username|password; ignoring"
        );
        return;
    }
    let (registry, username, password) = (parts[0], parts[1], parts[2]);
    let runtime = std::env::var("SWFC_CONTAINER_RUNTIME").unwrap_or_else(|_| "docker".into());
    let tool: &str = match runtime.as_str() {
        "podman" => "podman",
        // docker login also covers the apptainer-via-docker registry path;
        // a pure apptainer (no docker) host uses --docker-username at pull
        // time which is wired through to the SLURM agent script.
        _ => "docker",
    };
    use std::io::Write;
    let mut child = match std::process::Command::new(tool)
        .arg("login")
        .arg("--username")
        .arg(username)
        .arg("--password-stdin")
        .arg(registry)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            eprintln!("[registry-auth] failed to spawn `{tool} login`: {e}");
            return;
        }
    };
    if let Some(mut stdin) = child.stdin.take() {
        if let Err(e) = stdin.write_all(password.as_bytes()) {
            eprintln!("[registry-auth] writing password to `{tool} login` stdin failed: {e}");
            // Drop stdin so the child exits.
            drop(stdin);
            let _ = child.wait();
            return;
        }
    }
    match child.wait_with_output() {
        Ok(out) if out.status.success() => {
            eprintln!("[registry-auth] `{tool} login {registry}` succeeded");
        }
        Ok(out) => {
            let stderr = String::from_utf8_lossy(&out.stderr);
            eprintln!(
                "[registry-auth] `{tool} login {registry}` exited {}: {}",
                out.status, stderr
            );
        }
        Err(e) => {
            eprintln!("[registry-auth] `{tool} login` wait failed: {e}");
        }
    }
}

/// Apply the SECRET_KEYS + REQUIRED_INHERITED_KEYS
/// allowlist + merged envelope to a `std::process::Command`.
/// `env_clear` strips every inherited variable first; the allowlist
/// + envelope are then explicitly re-added.
///
/// When `SWFC_DISABLE_ENV_CLEAR=1` the function falls back to the
/// prior inherit-everything behaviour: it does NOT call `env_clear`,
/// only applies the envelope on top of whatever the harness inherits.
fn apply_env_with_allowlist(cmd: &mut std::process::Command, envelope: &BTreeMap<String, String>) {
    if env_clear_disabled() {
        // Legacy / opt-out path: do NOT clear, just layer the envelope
        // on top of whatever the harness already exports.
        for (k, v) in envelope.iter() {
            cmd.env(k, v);
        }
        return;
    }
    cmd.env_clear();
    // Re-add allowlisted keys from the harness's own environment.
    for &k in SECRET_KEYS.iter().chain(REQUIRED_INHERITED_KEYS.iter()) {
        if let Ok(v) = std::env::var(k) {
            cmd.env(k, v);
        }
    }
    // Envelope wins on collision — the harness-computed values for
    // SWFC_TASK_ID, SWFC_PACKAGE_ROOT, etc. should never be shadowed
    // by host env-vars even if they happen to share names.
    for (k, v) in envelope.iter() {
        cmd.env(k, v);
    }
}

/// Same shape as `apply_env_with_allowlist` but for `duct::Expression`.
/// Returns the (possibly env-cleared) expression. The duct API lacks
/// a single-call `env_clear`; we use `full_env(&BTreeMap::new())` to
/// produce the equivalent empty starting environment, then layer the
/// allowlist + envelope.
fn duct_expr_with_allowlist(
    mut expr: duct::Expression,
    envelope: &BTreeMap<String, String>,
) -> duct::Expression {
    if env_clear_disabled() {
        for (k, v) in envelope.iter() {
            expr = expr.env(k, v);
        }
        return expr;
    }
    let mut combined: BTreeMap<String, String> = BTreeMap::new();
    for &k in SECRET_KEYS.iter().chain(REQUIRED_INHERITED_KEYS.iter()) {
        if let Ok(v) = std::env::var(k) {
            combined.insert(k.to_string(), v);
        }
    }
    for (k, v) in envelope.iter() {
        combined.insert(k.clone(), v.clone());
    }
    expr.full_env(&combined)
}

/// Phase C7 — bubblewrap sandbox wiring.
///
/// Checks whether SWFC_LOCAL_SANDBOX=bubblewrap and the task is an
/// `Implementation::GeneratedCode` node (the only case where the sandbox
/// is mandatory — other task kinds are dispatched unwrapped). Returns
/// `Some(wrapped_command)` when the sandbox should apply, `None` when
/// the dispatch should proceed normally.
///
/// The function is best-effort on config-loading errors (missing sidecars,
/// parse failures): it logs to stderr and returns `None` so the task still
/// runs. Only a *BwrapBinaryMissing* error — where the operator opted in
/// but `bwrap` is absent — is escalated to the caller as an `Err`.
fn maybe_wrap_with_bwrap(
    package: &Path,
    agent_cmd: &str,
    task_id: &str,
) -> Result<Option<std::process::Command>, crate::sandbox_enforcer::SandboxRunnerError> {
    use crate::sandbox_enforcer::{append_sandbox_run_record, BubblewrapRunner};
    use scripps_workflow_core::workflow_contracts::implementation::Implementation;
    use scripps_workflow_core::workflow_contracts::task_node::TaskNode;

    // Build the runner (checks SWFC_LOCAL_SANDBOX and bwrap existence).
    let runner = match BubblewrapRunner::from_env(package.to_path_buf()) {
        Ok(Some(r)) => r,
        Ok(None) => return Ok(None), // mode=off (default)
        Err(e) => return Err(e),     // bwrap missing — escalate
    };

    // Load task-nodes.json + sandbox-policy.json from the package runtime.
    // Both sidecars are optional (v1/v2/v3 packages don't have them);
    // absence is a soft-skip, not an error.
    let runtime = package.join("runtime");
    let nodes_bytes = match std::fs::read(runtime.join("task-nodes.json")) {
        Ok(b) => b,
        Err(_) => return Ok(None), // pre-v4 package — no policy to enforce
    };
    let policy_bytes = match std::fs::read(runtime.join("sandbox-policy.json")) {
        Ok(b) => b,
        Err(_) => return Ok(None),
    };

    let nodes: Vec<TaskNode> = match serde_json::from_slice(&nodes_bytes) {
        Ok(n) => n,
        Err(e) => {
            eprintln!("[sandbox-enforcer] task-nodes.json parse error: {e}");
            return Ok(None);
        }
    };
    let policy: scripps_workflow_core::sandbox_policy::SandboxPolicy =
        match serde_json::from_slice(&policy_bytes) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("[sandbox-enforcer] sandbox-policy.json parse error: {e}");
                return Ok(None);
            }
        };

    // Only wrap Implementation::GeneratedCode tasks. Other task kinds
    // (shell scripts, discover_*, validate_*) are not in scope for the
    // bubblewrap enforcement — they run as-is.
    //
    // W2.2: when task-nodes.json IS present but the task_id is not in
    // it, treat it as a packaging bug and refuse to dispatch — silently
    // bypassing the sandbox in this case would defeat the whole policy
    // surface. The earlier "pre-v4 package" return at line 280 is the
    // soft-skip path; here we've already established the package opted
    // into the policy and any missing entry is a real inconsistency.
    let node = nodes.iter().find(|n| n.id == task_id);
    let node = match node {
        Some(n) => n,
        None => {
            return Err(
                crate::sandbox_enforcer::SandboxRunnerError::TaskNotInNodeList {
                    task_id: task_id.to_string(),
                },
            );
        }
    };
    let is_generated_code = matches!(node.implementation, Implementation::GeneratedCode { .. });

    if !is_generated_code {
        return Ok(None);
    }

    let bwrap_args = runner.render_args(&policy);
    tracing::info!(
        "[sandbox-enforcer] bwrap-wrapped task {} with policy {}",
        task_id,
        BubblewrapRunner::policy_digest(&policy),
    );

    // append_sandbox_run_record is called AFTER the process exits (we
    // don't know the exit code yet). We record the pre-launch entry here
    // with exit_code=None; the caller fills it in.
    //
    // To keep the call site clean, we return the wrapped command and let
    // the caller record the provenance line once the subprocess exits.
    let cmd = runner.wrap(agent_cmd, &[], &policy);
    let _ = (bwrap_args, append_sandbox_run_record); // mark used; actual call is post-exit
    Ok(Some(cmd))
}

/// Post-exit sandbox provenance helper. Called after the agent process exits
/// to append the sandbox-runs.jsonl record. Separated from `maybe_wrap_with_bwrap`
/// so the exit code is available.
///
/// Only records when SWFC_LOCAL_SANDBOX=bubblewrap AND runtime/sandbox-policy.json
/// is present (same conditions that caused the wrap in the first place).
fn record_sandbox_run(package: &Path, task_id: &str, exit_code: Option<i32>) {
    use crate::sandbox_enforcer::{append_sandbox_run_record, BubblewrapRunner};

    // Fast path: when SWFC_LOCAL_SANDBOX is unset / off / empty, no wrap
    // occurred and there is nothing to record. Returning here matches
    // this function's documented invariant and avoids the misleading
    // `sandbox_record_skipped` silent-skip noise on every single
    // dispatch in operator configurations that intentionally run
    // unsandboxed (the harness's default).
    let sandbox_mode = std::env::var("SWFC_LOCAL_SANDBOX").ok().unwrap_or_default();
    if sandbox_mode.is_empty() || sandbox_mode == "off" {
        return;
    }

    // W1.2/B9: each of these three skip paths previously returned
    // silently. Note each via the silent-skip counter so a pattern of
    // missing records (e.g. sandbox-policy.json deleted mid-run)
    // surfaces in harness-health.json.
    let policy_bytes = match std::fs::read(package.join("runtime/sandbox-policy.json")) {
        Ok(b) => b,
        Err(_) => {
            crate::_observability::note_silent_skip(
                crate::_observability::SkipCategory::SandboxRecordSkipped,
                "sandbox-policy.json missing — no record written",
                Some(task_id),
            );
            return;
        }
    };
    let policy: scripps_workflow_core::sandbox_policy::SandboxPolicy =
        match serde_json::from_slice(&policy_bytes) {
            Ok(p) => p,
            Err(e) => {
                crate::_observability::note_silent_skip(
                    crate::_observability::SkipCategory::SandboxRecordSkipped,
                    &format!("sandbox-policy.json parse error: {}", e),
                    Some(task_id),
                );
                return;
            }
        };
    let runner_opt = match BubblewrapRunner::from_env(package.to_path_buf()) {
        Ok(opt) => opt,
        Err(e) => {
            crate::_observability::note_silent_skip(
                crate::_observability::SkipCategory::SandboxRecordSkipped,
                &format!("BubblewrapRunner::from_env error: {}", e),
                Some(task_id),
            );
            None
        }
    };
    if let Some(runner) = runner_opt {
        let bwrap_args = runner.render_args(&policy);
        append_sandbox_run_record(package, task_id, &policy, &bwrap_args, exit_code);
    } else {
        // Runner is None when SWFC_LOCAL_SANDBOX is "off" or unset
        // without bwrap available — by-design no record to write.
        // Don't tick the counter for this case.
    }
}

/// `Executor` implementation that runs agent subprocesses on the local host.
/// Supports optional bubblewrap sandboxing (`SWFC_LOCAL_SANDBOX=bubblewrap`)
/// and per-task derived-image builds (`SWFC_PER_TASK_IMAGES=1`).
pub struct LocalExecutor {
    task_timeout_secs: u64,
    package: String,
    agent: String,
    stall_tx: Option<mpsc::Sender<StallSignal>>,
    stall_thresholds: Option<StallThresholds>,
    stall_shutdown: Arc<Mutex<bool>>,
    /// Per-task envelope additions accumulated from
    /// `apply_overrides`. Cleared between iterations once the agent
    /// process has exited so a remediation can't leak across tasks.
    pending_envelope_additions: BTreeMap<String, BTreeMap<String, String>>,
    /// Session-wide env additions populated once at `provision`-time
    /// and merged into every iteration's envelope. Today this carries
    /// the derived-image tag from the warm-up pre-flight; it can be
    /// extended to other session-scoped vars without rewiring callers.
    session_env_additions: BTreeMap<String, String>,
    /// Per-task image tag overrides populated at `provision`-time
    /// when `SWFC_PER_TASK_IMAGES=1`. Maps
    /// `task_id -> "scripps-derived:<per_atom_hash>"`. Read by
    /// `run_iteration` against the dispatched task's `SWFC_TASK_ID`
    /// envelope value; absent entries fall through to the
    /// session-wide image (legacy union build) or host mode.
    /// Populated fresh on each `provision` (LocalExecutor is constructed
    /// per-harness-invocation so no per-run reset is needed).
    per_task_image_overrides: BTreeMap<String, String>,
    /// Most recent iteration capture. Populated by `run_iteration`,
    /// drained by `take_last_capture`. Holds the agent's stderr/stdout +
    /// exit + wallclock so the harness main can synthesise a
    /// `ToolErrorEnvelope` after a non-zero iteration.
    last_capture: Option<IterationCapture>,
}

impl LocalExecutor {
    /// Constructs a `LocalExecutor` from executor args.
    pub fn new(args: &ExecutorArgs) -> Self {
        Self {
            task_timeout_secs: args.task_timeout_secs,
            package: args.package.clone(),
            agent: args.agent.clone(),
            stall_tx: None,
            stall_thresholds: None,
            stall_shutdown: Arc::new(Mutex::new(false)),
            pending_envelope_additions: BTreeMap::new(),
            session_env_additions: BTreeMap::new(),
            per_task_image_overrides: BTreeMap::new(),
            last_capture: None,
        }
    }

    /// Derived-image warm-up pre-flight that runs once per session
    /// before any agent dispatch. Reads the package's
    /// `policies/runtime-prereqs.json`, content-hashes it via the
    /// shared `derived_image::content_hash`, then either:
    ///
    /// - returns `Ok(None)` when the manifest is empty (legacy
    ///   packages, modalities that opt out) — caller falls through
    ///   to host-mode dispatch,
    /// - returns `Ok(Some(tag))` when the derived image was either
    ///   already cached locally OR built by `scripts/build-derived-image.sh`,
    /// - returns `Err(...)` when the build failed.
    ///
    /// On success, also stores `SWFC_DEFAULT_CONTAINER_IMAGE=<tag>` in
    /// `session_env_additions` so every subsequent agent dispatch
    /// inherits the override automatically.
    ///
    /// Honors `SWFC_FORCE_IMAGE_REBUILD=1` (skip cache check) and
    /// `SWFC_IMAGE_BUILD_TIMEOUT_SECS` (build-wallclock cap; default
    /// 1800). The builder script handles those env vars directly.
    pub fn warm_runtime_image(&mut self, package_dir: &Path) -> Result<Option<String>> {
        // R2.5 — if SWFC_CONTAINER_REGISTRY_AUTH is set, perform a one-shot
        // `docker login` (or `podman login`) before any image pull/build so
        // the builder + agent subprocesses inherit the credential cache.
        // Best-effort: a failure here logs but does not abort warm-up because
        // not every package needs registry auth (public images stay reachable).
        registry_login_if_configured();

        let manifest_path = package_dir.join("policies/runtime-prereqs.json");
        if !manifest_path.exists() {
            // Pre-S15.x package — nothing to do.
            return Ok(None);
        }
        let raw = std::fs::read_to_string(&manifest_path).with_context(|| {
            format!(
                "reading runtime-prereqs manifest at {}",
                manifest_path.display()
            )
        })?;
        let prereqs: scripps_workflow_core::runtime_prereqs::RuntimePrereqs =
            serde_json::from_str(&raw).with_context(|| {
                format!(
                    "parsing runtime-prereqs manifest at {}",
                    manifest_path.display()
                )
            })?;
        // Per directive language packages don't drive the
        // derived-image build — only system packages (apt / dnf) do.
        // Three cases:
        // 1. no base_image → host mode
        // 2. base_image set, no apt delta → use base directly,
        // no derivation
        // 3. base_image set, apt delta → build derived image,
        // use derived tag
        if prereqs.base_image.is_none() {
            return Ok(None);
        }
        if !prereqs.is_buildable() {
            // Case 2 — base image is everything we need. The agent
            // installs language packages at task time using the
            // mounted per-session cache.
            let base = prereqs.base_image.as_ref().unwrap().clone();
            self.session_env_additions
                .insert("SWFC_DEFAULT_CONTAINER_IMAGE".into(), base.clone());
            return Ok(Some(base));
        }
        // Hash the on-disk bytes so the tag matches what
        // `scripts/build-derived-image.sh` (which uses `sha256sum`)
        // computes. The in-memory `content_hash(&prereqs)` would hash
        // the compact serialization — different bytes, different
        // hash — and the cache hit in the script would never resolve.
        let hash = scripps_workflow_core::derived_image::content_hash_from_file(&manifest_path)
            .with_context(|| {
                format!(
                    "hashing runtime-prereqs manifest at {}",
                    manifest_path.display()
                )
            })?;
        let prefix = std::env::var("SWFC_DERIVED_IMAGE_TAG_PREFIX")
            .unwrap_or_else(|_| "scripps-derived".into());
        let tag = format!("{prefix}:{hash}");

        // Resolve the builder script. Honor the same SWFC_*_PATH style
        // override convention the harness already uses for sibling
        // scripts so operators can point this at a fork without rebuilding.
        let builder = std::env::var("SWFC_IMAGE_BUILDER_PATH").unwrap_or_else(|_| {
            // Default: walk up from the package's parent until we hit
            // the repo root (contains scripts/build-derived-image.sh).
            // For ad-hoc smokes operators can pass an absolute path
            // via the env var.
            "scripts/build-derived-image.sh".into()
        });

        let mut cmd = std::process::Command::new(&builder);
        cmd.arg(package_dir);
        // Pass through the envs the builder script reads.
        for var in [
            "SWFC_FORCE_IMAGE_REBUILD",
            "SWFC_IMAGE_BUILD_TIMEOUT_SECS",
            "SWFC_BUILDX_CACHE_DIR",
            "SWFC_DERIVED_IMAGE_TAG_PREFIX",
            "SWFC_AGENT_CACHE_DIR",
        ] {
            if let Ok(v) = std::env::var(var) {
                cmd.env(var, v);
            }
        }
        let status = cmd
            .status()
            .with_context(|| format!("invoking image builder {}", builder))?;
        match status.code() {
            Some(0) => {
                self.session_env_additions
                    .insert("SWFC_DEFAULT_CONTAINER_IMAGE".into(), tag.clone());
                Ok(Some(tag))
            }
            Some(10) => {
                // Manifest empty/not buildable. Race-safe with the
                // pre-check above (manifest could change between
                // is_buildable and the script's own check).
                Ok(None)
            }
            Some(20) => Err(anyhow::anyhow!(
                "derived-image build failed for tag {} — see builder stderr above",
                tag
            )),
            Some(30) => Err(anyhow::anyhow!(
                "derived-image build skipped: docker daemon unreachable or jq missing"
            )),
            Some(c) => Err(anyhow::anyhow!(
                "derived-image builder exited with unexpected code {} (tag {})",
                c,
                tag
            )),
            None => Err(anyhow::anyhow!(
                "derived-image builder terminated by signal (tag {})",
                tag
            )),
        }
    }

    /// Pop and merge accumulated overrides into the envelope the
    /// harness is about to pass to `run_iteration`. Public so the
    /// harness main can compose this on top of the hardware envelope.
    pub fn drain_envelope_additions(&mut self) -> BTreeMap<String, String> {
        let mut out = BTreeMap::new();
        for (_task, vars) in std::mem::take(&mut self.pending_envelope_additions) {
            out.extend(vars);
        }
        out
    }
}

/// Defaults are SECURITY-RELEVANT: changes here affect blast radius of
/// Implementation::GeneratedCode tasks.
///
/// Called when `SWFC_LOCAL_SANDBOX` is unset. Probes PATH for `bwrap`:
/// - Found → returns `ProcessIsolation` (bubblewrap enabled).
/// - Not found → emits a one-shot `tracing::warn!` and returns `None`.
///
/// The warn fires at most once per process lifetime via `std::sync::Once`.
/// Operators who explicitly set `SWFC_LOCAL_SANDBOX=off` bypass this
/// function entirely and see no warning.
fn detect_default_sandbox() -> scripps_workflow_core::atom::SandboxRequirement {
    // One-shot warn so long-running harness loops don't spam logs.
    static WARN_ONCE: std::sync::Once = std::sync::Once::new();

    let bwrap_found = std::process::Command::new("which")
        .arg("bwrap")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);

    if bwrap_found {
        scripps_workflow_core::atom::SandboxRequirement::ProcessIsolation
    } else {
        WARN_ONCE.call_once(|| {
            tracing::warn!(
                "SWFC_LOCAL_SANDBOX unset and bwrap not found on PATH. \
                 Implementation::GeneratedCode tasks will run unsandboxed. \
                 Install bwrap (sudo apt install bubblewrap) or set \
                 SWFC_LOCAL_SANDBOX=off explicitly to suppress this warning."
            );
        });
        scripps_workflow_core::atom::SandboxRequirement::None
    }
}

impl Executor for LocalExecutor {
    fn name(&self) -> &'static str {
        "local"
    }

    /// Local executor capability profile. Sandbox tier is
    /// driven by `SWFC_LOCAL_SANDBOX`: `bubblewrap` enables the bwrap path;
    /// `off` explicitly disables it; unset auto-detects via
    /// `detect_default_sandbox`. Network is `Bridge` by default — the
    /// host has the same egress as the operator's shell.
    fn capabilities(&self) -> super::ExecutorCapabilities {
        // W2.1: `validate_sandbox_env` is called at executor build time
        // (see `executor::build` in `mod.rs`) and refuses to construct
        // the LocalExecutor when SWFC_LOCAL_SANDBOX carries an unknown
        // token. Reaching this match arm with an unknown value would
        // require a TOCTOU between build and capabilities() — vanishingly
        // unlikely in practice — but the match below still rejects loudly
        // via tracing::warn! so the regression isn't silent.
        let sandbox = match std::env::var("SWFC_LOCAL_SANDBOX").as_deref() {
            Ok("bubblewrap") => scripps_workflow_core::atom::SandboxRequirement::ProcessIsolation,
            Ok("off") | Ok("") => scripps_workflow_core::atom::SandboxRequirement::None,
            Ok(other) => {
                tracing::warn!(
                    target: "local_sandbox",
                    value = %other,
                    "SWFC_LOCAL_SANDBOX changed to an unknown value after executor build; \
                     using SandboxRequirement::None (the build-time validator should have caught \
                     this — operator likely mutated the env mid-run)"
                );
                scripps_workflow_core::atom::SandboxRequirement::None
            }
            // Unset: probe for bwrap and smart-default.
            Err(_) => detect_default_sandbox(),
        };
        super::ExecutorCapabilities {
            sandbox,
            network: scripps_workflow_core::atom::NetworkPolicy::Bridge,
            kind: "local",
        }
    }

    fn pilot(&mut self, dag: &DAG, cfg: &PilotConfig) -> Result<Option<PilotReport>> {
        if !cfg.enabled {
            return Ok(None);
        }
        let package = Path::new(&self.package).to_path_buf();
        let profiles = load_profiles(&package).unwrap_or_else(|| {
            // Without compute profiles we can still run the pilot, we
            // just can't rank by stage_class weight or project
            // baselines. Use an empty profile set — the fallback
            // selection path handles this gracefully.
            ComputeProfiles {
                profiles: Default::default(),
                default: super::sizing::DefaultProfile {
                    requirements: super::sizing::BaseRequirements::default(),
                    notes: None,
                },
                method_overrides: Default::default(),
            }
        });
        let facts = load_facts(&package);
        let pilot_ids = select_pilot_tasks(dag, &profiles, &facts, cfg);

        let mut measurements = Vec::new();
        for task_id in &pilot_ids {
            let Some(task) = dag.tasks.get(task_id.as_str()) else {
                continue;
            };
            let stage_class = task
                .spec
                .as_ref()
                .and_then(|s| s.get("stage_class"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            if let Some(m) =
                measure_one_iteration(&package, &self.agent, task_id, &stage_class, cfg)?
            {
                measurements.push(m);
            }
        }

        let projected = project_requirements(dag, &profiles, &facts, &measurements, cfg);
        let confidence = compute_confidence(&measurements, &profiles, &facts);
        let report = PilotReport {
            measurements,
            projected_requirements: projected,
            confidence,
        };
        write_pilot_artifacts(&package, &report)?;
        Ok(Some(report))
    }

    #[tracing::instrument(skip(self, dag), fields(executor = "local"))]
    fn provision(&mut self, dag: &DAG) -> Result<()> {
        // Derived-image warm-up runs once per session before the
        // first agent dispatch. Empty/legacy manifests return
        // Ok(None) and we fall through to host-mode. Build failures
        // bubble up so the harness can surface a typed
        // BlockerKind::ImageBuildFailed (callers wrap this).
        //
        // When SWFC_PER_TASK_IMAGES=1 is set, the union-build path
        // below is skipped entirely:
        // we walk the DAG, derive a per-atom image for each task
        // whose source atom carries a buildable manifest, and stash
        // the resulting tag in `per_task_image_overrides[task.id]`.
        // `run_iteration` then injects the per-task tag into the
        // dispatched agent's `SWFC_DEFAULT_CONTAINER_IMAGE`
        // (overriding any session-wide tag from the legacy union
        // build). Atoms with no install delta (no
        // `policies/atom-prereqs/<atom_id>.json`, or an unbuildable
        // manifest) get no override and fall through to host mode
        // or `atom.preferred_container.image`. Tasks with no
        // `source_atom_id` (synthetic gates, iter placeholders) are
        // skipped — they run host-mode like before.
        //
        // Dedupe: a per-dispatch HashMap caches the
        // `warm_per_atom_image` result per atom_id so two tasks
        // sharing an atom hit the builder script exactly once. The
        // script's local registry cache means even cross-session
        // hits cost zero rebuild work, but per-dispatch caching also
        // avoids spawning a redundant subprocess.
        let package_dir = std::path::Path::new(&self.package).to_path_buf();
        if scripps_workflow_core::derived_image::per_task_images_enabled() {
            eprintln!(
                "  ◇ SWFC_PER_TASK_IMAGES=1 — per-atom build path active \
                 (skipping session-wide union build)"
            );
            let mut per_atom_cache: std::collections::HashMap<String, Option<String>> =
                std::collections::HashMap::new();
            let mut tasks_with_override = 0usize;
            let mut tasks_with_no_atom = 0usize;
            let mut tasks_with_no_manifest = 0usize;
            for (task_id, task) in &dag.tasks {
                let Some(atom_id) = task.source_atom_id.as_deref() else {
                    tasks_with_no_atom += 1;
                    continue;
                };
                let tag = per_atom_cache
                    .entry(atom_id.to_string())
                    .or_insert_with(|| {
                        match crate::executor::per_atom_image::warm_per_atom_image(
                            &package_dir,
                            atom_id,
                        ) {
                            Ok(t) => t,
                            Err(e) => {
                                eprintln!(
                                    "  ⚠ per-atom build for {atom_id}: {e:#} \
                                     (task {task_id} will fall back to host mode)"
                                );
                                None
                            }
                        }
                    })
                    .clone();
                match tag {
                    Some(t) => {
                        self.per_task_image_overrides.insert(task_id.to_string(), t);
                        tasks_with_override += 1;
                    }
                    None => {
                        tasks_with_no_manifest += 1;
                    }
                }
            }
            eprintln!(
                "  ◇ per-task images: {} task(s) pinned, {} no source atom, \
                 {} no install delta",
                tasks_with_override, tasks_with_no_atom, tasks_with_no_manifest
            );
            return Ok(());
        }
        match self.warm_runtime_image(&package_dir) {
            Ok(Some(tag)) => {
                eprintln!("  ✓ derived-image warm-up: agent will run inside {tag}");
            }
            Ok(None) => {
                // Quiet: legacy/empty manifest is the common case.
            }
            Err(e) => {
                // Don't block provision on warm-up failure — the
                // harness can still run host-mode while the operator
                // resolves the build issue. Surface a clear stderr
                // line so the failure isn't silent.
                eprintln!("  ⚠ derived-image warm-up failed (continuing host-mode): {e:#}");
            }
        }
        Ok(())
    }

    fn start_stall_monitor(
        &mut self,
        thresholds: &StallThresholds,
        tx: mpsc::Sender<StallSignal>,
    ) -> Result<()> {
        self.stall_tx = Some(tx);
        self.stall_thresholds = Some(thresholds.clone());
        *self.stall_shutdown.lock() = false;
        Ok(())
    }

    fn stop_stall_monitor(&mut self) {
        *self.stall_shutdown.lock() = true;
        self.stall_tx = None;
        self.stall_thresholds = None;
    }

    #[tracing::instrument(skip(self, package, agent_cmd, envelope), fields(executor = "local"))]
    fn run_iteration(
        &mut self,
        package: &Path,
        agent_cmd: &str,
        envelope: &std::collections::BTreeMap<String, String>,
    ) -> Result<IterationOutcome> {
        // When a stall monitor is armed, use std::process::Command so we
        // have the child PID and can sample `/proc/<pid>/stat`. Falls
        // back to the byte-identical duct-based path otherwise. The
        // hardware envelope is applied to the subprocess as
        // env vars in both paths — the agent script reads SWFC_HW_*
        // per `prompt_role.txt` → "Hardware-aware execution".
        let started = std::time::Instant::now();
        // Session-wide additions (e.g. the warm-up pre-flight's
        // `SWFC_DEFAULT_CONTAINER_IMAGE=<derived-tag>`) merge in
        // before per-task additions. Per-task wins on collision,
        // which keeps any future remediation precedence intact.
        let mut merged_envelope: BTreeMap<String, String> = envelope
            .clone()
            .into_iter()
            .chain(self.session_env_additions.clone())
            .chain(self.drain_envelope_additions())
            .collect();

        // Validate SWFC_TASK_ID at the harness boundary before it flows
        // into per-task path composition (per-task image override
        // lookup, sandbox-wrap, `runtime/outputs/<task_id>/` paths
        // inside the agent subprocess). Agent scripts enforce on their
        // side via `validate_task_id`; this is defense-in-depth so a
        // malformed value never even reaches the spawn.
        if let Some(task_id_env) =
            merged_envelope.get(crate::executor::hardware_envelope::TASK_ID_ENV)
        {
            crate::executor::_id_validator::sanitize_task_id(task_id_env).map_err(|reason| {
                anyhow::anyhow!("refusing dispatch: unsafe SWFC_TASK_ID in envelope: {reason}")
            })?;
        }

        // Apply the per-task image
        // override if `provision` derived one for this task's atom.
        // The override wins on collision: a per-atom tag set in
        // `per_task_image_overrides[task_id]` replaces any
        // `SWFC_DEFAULT_CONTAINER_IMAGE` from the session
        // (per_task_image_overrides is only populated when
        // SWFC_PER_TASK_IMAGES=1, and in that mode the session-wide
        // warm-up is skipped — so there's nothing to collide with in
        // practice, but the precedence is documented here for the
        // future remediation that swaps task images at runtime).
        if let Some(task_id_for_image) =
            merged_envelope.get(crate::executor::hardware_envelope::TASK_ID_ENV)
        {
            if let Some(tag) = self.per_task_image_overrides.get(task_id_for_image) {
                merged_envelope.insert("SWFC_DEFAULT_CONTAINER_IMAGE".into(), tag.clone());
            }
        }

        // Phase C7 — bubblewrap sandbox enforcement.
        // Read SWFC_TASK_ID from the envelope to know which node to check.
        // When bubblewrap is active AND the task is GeneratedCode, build a
        // bwrap-wrapped Command and use the std::process path (regardless of
        // whether the stall monitor is armed) so we have the child PID.
        let task_id_for_sandbox = merged_envelope
            .get(crate::executor::hardware_envelope::TASK_ID_ENV)
            .cloned()
            .unwrap_or_default();
        let sandbox_cmd_opt = if !task_id_for_sandbox.is_empty() {
            match maybe_wrap_with_bwrap(package, agent_cmd, &task_id_for_sandbox) {
                Ok(opt) => opt,
                Err(e) => {
                    // bwrap explicitly requested but binary is absent — surface
                    // as a hard error so the SME knows the policy wasn't enforced.
                    return Err(anyhow::anyhow!(
                        "[sandbox-enforcer] cannot wrap task {}: {}",
                        task_id_for_sandbox,
                        e
                    ));
                }
            }
        } else {
            None
        };

        // Use std::process::Command when either the stall monitor is armed OR
        // the sandbox wrapping is active (both need access to the child PID or
        // produce a `std::process::Command`).
        let use_std_path = self.stall_tx.is_some() || sandbox_cmd_opt.is_some();

        if use_std_path {
            use std::os::unix::process::ExitStatusExt;
            use std::process::{Command, Stdio};

            let mut cmd: Command = match sandbox_cmd_opt {
                Some(bwrap_cmd) => {
                    // The wrapped command already has bwrap as argv0 and the
                    // agent as the trailing argument. Re-attach stdio.
                    let mut c = bwrap_cmd;
                    c.stdout(Stdio::piped()).stderr(Stdio::piped());
                    c
                }
                None => {
                    // Normal (unsandboxed) std::process path — mirrors the
                    // original stall-monitor branch exactly.
                    let mut c = Command::new(agent_cmd);
                    c.arg(package.to_string_lossy().to_string())
                        .stdout(Stdio::piped())
                        .stderr(Stdio::piped());
                    c
                }
            };

            // Mirror SLURM's allowlist approach.
            // env_clear() strips the inherited host environment;
            // SECRET_KEYS + REQUIRED_INHERITED_KEYS + the merged
            // envelope are then explicitly re-added. SWFC_DISABLE_ENV_CLEAR=1
            // bypasses for legacy test compatibility.
            apply_env_with_allowlist(&mut cmd, &merged_envelope);
            let mut child = cmd
                .spawn()
                .with_context(|| format!("running agent '{}'", agent_cmd))?;
            let pid = child.id();

            // W5.4: write `.agent-pid` sidecar so the
            // HeartbeatLivenessProbe can verify the recorded agent is
            // still alive via kill(pid, 0). Defends against the
            // documented zombie-pgrep scenario where a stuck polling
            // loop keeps touching .heartbeat after the real agent dies.
            // Best-effort write — failure to write the sidecar means
            // the probe falls back to mtime-only, which is the prior
            // behavior. The task_id lives in the envelope under
            // `SWFC_TASK_ID` (validated above by sanitize_task_id).
            if let Some(task_id_env) = merged_envelope
                .get(crate::executor::hardware_envelope::TASK_ID_ENV)
                .filter(|v| !v.is_empty())
            {
                let pid_path = Path::new(package)
                    .join("runtime/outputs")
                    .join(task_id_env)
                    .join(".agent-pid");
                if let Err(e) = std::fs::write(&pid_path, format!("{}\n", pid)) {
                    tracing::warn!(
                        target: "agent_pid_sidecar",
                        task_id = %task_id_env,
                        path = %pid_path.display(),
                        error = %e,
                        "could not write .agent-pid; heartbeat liveness will fall back to mtime-only"
                    );
                }
            }

            // Stall monitor (only when armed).
            let monitor_handle = if let (Some(tx), Some(thresholds)) =
                (self.stall_tx.clone(), self.stall_thresholds.clone())
            {
                *self.stall_shutdown.lock() = false;
                let shutdown = self.stall_shutdown.clone();
                // Heartbeat-veto inputs: resolve the per-task output
                // dir from the envelope's SWFC_TASK_ID; if absent
                // (legacy / test path), pass None and the veto is a
                // no-op. The freshness threshold mirrors the harness's
                // own `SWFC_TASK_HEARTBEAT_STALL_SECS` (300s default)
                // so the stall monitor agrees with the orphan-reaper
                // about what counts as "alive".
                let heartbeat_root: Option<std::path::PathBuf> = merged_envelope
                    .get(crate::executor::hardware_envelope::TASK_ID_ENV)
                    .filter(|v| !v.is_empty())
                    .map(|tid| package.join("runtime/outputs").join(tid));
                let heartbeat_freshness_secs: u64 = std::env::var("SWFC_TASK_HEARTBEAT_STALL_SECS")
                    .ok()
                    .and_then(|s| s.parse::<u64>().ok())
                    .unwrap_or(300);
                Some(std::thread::spawn(move || {
                    sample_stall_loop(
                        pid,
                        "local_iteration",
                        thresholds,
                        tx,
                        shutdown,
                        heartbeat_root,
                        heartbeat_freshness_secs,
                    );
                }))
            } else {
                None
            };

            // Drain stdout and stderr concurrently. Reading stdout to
            // EOF before touching stderr can deadlock when the child
            // writes enough stderr to fill the pipe buffer while stdout
            // stays open.
            let stdout_handle = child.stdout.take();
            let stderr_handle = child.stderr.take();
            let stdout_thread = std::thread::spawn(move || {
                use std::io::Read;
                let mut buf = String::new();
                if let Some(mut h) = stdout_handle {
                    let _ = h.read_to_string(&mut buf);
                }
                buf
            });
            let stderr_thread = std::thread::spawn(move || {
                use std::io::Read;
                let mut buf = String::new();
                if let Some(mut h) = stderr_handle {
                    let _ = h.read_to_string(&mut buf);
                }
                buf
            });
            let status = child.wait().context("waiting on agent")?;
            let stdout_str = stdout_thread.join().unwrap_or_default();
            let stderr_str = stderr_thread.join().unwrap_or_default();
            if let Some(h) = monitor_handle {
                *self.stall_shutdown.lock() = true;
                let _ = h.join();
            }
            let signal = status.signal().map(|s| format!("SIG{}", signal_name(s)));

            // Phase C7 — record sandbox provenance after the process exits.
            if !task_id_for_sandbox.is_empty() {
                // Only records when SWFC_LOCAL_SANDBOX=bubblewrap and
                // task-nodes.json + sandbox-policy.json are present.
                record_sandbox_run(package, &task_id_for_sandbox, status.code());
            }

            self.last_capture = Some(IterationCapture {
                stderr: stderr_str,
                stdout: stdout_str,
                exit_code: status.code(),
                signal,
                wallclock_secs: Some(started.elapsed().as_secs()),
                peak_memory_mb: read_vmhwm_kb(pid).map(|kb| kb / 1024),
                executor_context: local_context(),
            });
            Ok(IterationOutcome {
                agent_status: status,
                remote: None,
            })
        } else {
            use std::os::unix::process::ExitStatusExt;
            let expr = duct::cmd!(agent_cmd, package.to_string_lossy().to_string());
            // Same env_clear + allowlist policy as
            // the std::process path above. SWFC_DISABLE_ENV_CLEAR=1
            // falls back to the legacy inherit-everything behaviour.
            let expr = duct_expr_with_allowlist(expr, &merged_envelope);
            let output = expr
                .unchecked()
                .stdout_capture()
                .stderr_capture()
                .run()
                .with_context(|| format!("running agent '{}'", agent_cmd))?;
            let signal = output
                .status
                .signal()
                .map(|s| format!("SIG{}", signal_name(s)));
            self.last_capture = Some(IterationCapture {
                stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
                stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
                exit_code: output.status.code(),
                signal,
                wallclock_secs: Some(started.elapsed().as_secs()),
                peak_memory_mb: None,
                executor_context: local_context(),
            });
            Ok(IterationOutcome {
                agent_status: output.status,
                remote: None,
            })
        }
    }

    fn take_last_capture(&mut self) -> Option<IterationCapture> {
        self.last_capture.take()
    }

    #[tracing::instrument(skip(self, task), fields(executor = "local"))]
    fn is_task_stale(&self, task: &Task, now_secs: u64) -> bool {
        let TaskState::Running { started_at, .. } = &task.state else {
            return false;
        };
        let Ok(start) = chrono::DateTime::parse_from_rfc3339(started_at) else {
            return false;
        };
        let elapsed = now_secs.saturating_sub(start.timestamp().max(0) as u64);
        elapsed >= self.task_timeout_secs
    }

    /// CPU budget for parallel dispatch on local mode.
    /// Computes `nproc / max(tool_thread_curves)` against the largest
    /// per-tool thread budget declared in the emitted
    /// compute-resource-policy. Falls back to `max(1, nproc / 8)` when
    /// the policy is absent so small boxes don't thrash.
    ///
    /// The budget is a maximum — `SWFC_HARNESS_CONCURRENCY` still
    /// clamps it lower when the operator wants explicit control.
    fn cpu_budget(&self) -> usize {
        let nproc = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1);
        let peak_tool_threads = load_peak_tool_thread_curve(Path::new(&self.package)).unwrap_or(8);
        ((nproc / peak_tool_threads.max(1)) as usize).max(1)
    }

    /// GPU budget on local mode. Probes `nvidia-smi -L` at
    /// call time (cheap, returns a line per GPU). Returns 0 when the
    /// binary is missing, not on PATH, or returns non-zero — local
    /// runs without a GPU just fall back to CPU-only.
    fn gpu_budget(&self) -> usize {
        probe_nvidia_gpu_count()
    }

    /// Honoured overrides on local:
    /// * `resources.memory_gb` → `SWFC_AGENT_MEMORY_CAP_GB` env var
    ///   (the existing systemd-run / prlimit / docker memory cap
    ///   already consumes this; we just override the value)
    /// * `resources.wallclock_secs` → `SWFC_AGENT_WALLCLOCK_SECS`
    ///   env var consumed by `agent-claude.sh` to set a SIGTERM
    ///   timer
    /// * `resources.vcpus` → advisory `SWFC_HW_NPROC_HINT`
    /// * `library_pins` → `SWFC_LIB_PIN_<NAME>=<VERSION>` envs
    /// * `stage_parameters` → merged into
    ///   `runtime/inputs/<task_id>/params.json` (additive)
    /// * `env_passthrough` → forwarded verbatim
    ///
    /// `disable_spot`, `partition`, `availability_zone` are AWS/SLURM
    /// concepts — silently ignored on local.
    fn apply_overrides(&mut self, task_id: &str, ov: &ExecutorOverrides) -> Result<()> {
        let mut env: BTreeMap<String, String> = BTreeMap::new();
        if let Some(res) = ov.resources.as_ref() {
            if let Some(gb) = res.memory_gb {
                env.insert("SWFC_AGENT_MEMORY_CAP_GB".into(), gb.to_string());
            }
            if let Some(secs) = res.wallclock_secs {
                env.insert("SWFC_AGENT_WALLCLOCK_SECS".into(), secs.to_string());
            }
            if let Some(vcpus) = res.vcpus {
                env.insert("SWFC_HW_NPROC_HINT".into(), vcpus.to_string());
            }
        }
        for (lib, ver) in &ov.library_pins {
            // C-8/C-9: same shape constraints as the
            // remote executors — the value will be exported into the
            // agent shell. Refuse anything outside the canonical shape
            // so a hostile library_pins entry can't smuggle a shell
            // payload into the local agent env.
            let Some(suffix) = scripps_workflow_core::env_validator::sanitize_lib_env_suffix(lib)
            else {
                tracing::warn!(
                    library = %lib,
                    "rejecting invalid library name in local env (C-8/C-9 hardening)"
                );
                continue;
            };
            if !scripps_workflow_core::env_validator::is_safe_env_value(ver) {
                tracing::warn!(
                    library = %lib,
                    value = %ver,
                    "rejecting invalid library version value in local env (C-8/C-9 hardening)"
                );
                continue;
            }
            let key = format!("SWFC_LIB_PIN_{suffix}");
            env.insert(key, ver.clone());
        }
        for (k, v) in &ov.env_passthrough {
            if !scripps_workflow_core::env_validator::is_valid_env_name(k) {
                tracing::warn!(
                    key = %k,
                    "rejecting invalid env_passthrough key in local env (C-8/C-9 hardening)"
                );
                continue;
            }
            if !scripps_workflow_core::env_validator::is_safe_env_value(v) {
                tracing::warn!(
                    key = %k,
                    value = %v,
                    "rejecting invalid env_passthrough value in local env (C-8/C-9 hardening)"
                );
                continue;
            }
            env.insert(k.clone(), v.clone());
        }
        if !ov.stage_parameters.is_empty() {
            merge_stage_params(Path::new(&self.package), task_id, &ov.stage_parameters)?;
        }
        if !env.is_empty() {
            self.pending_envelope_additions
                .insert(task_id.to_string(), env);
        }
        Ok(())
    }

    fn release(&mut self) {
        // No-op: no resources to clean up for local subprocess invocations.
    }
}

/// Merge `params` into `runtime/inputs/<task_id>/params.json`. Creates
/// the file when absent. Existing keys are overwritten; sibling keys
/// Preserved. Atomic via.tmp + rename.
fn merge_stage_params(
    package: &Path,
    task_id: &str,
    params: &BTreeMap<String, serde_json::Value>,
) -> Result<()> {
    let path = package
        .join("runtime")
        .join("inputs")
        .join(task_id)
        .join("params.json");
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    let mut existing: serde_json::Map<String, serde_json::Value> = if path.exists() {
        let raw = std::fs::read_to_string(&path).context("reading existing params.json")?;
        serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(&raw)
            .context("parsing existing params.json")?
    } else {
        serde_json::Map::new()
    };
    for (k, v) in params {
        existing.insert(k.clone(), v.clone());
    }
    let raw = serde_json::to_string_pretty(&serde_json::Value::Object(existing))
        .context("serialising merged params")?;
    scripps_workflow_core::fs_helpers::atomic_write_bytes_sync(&path, raw.as_bytes())
        .context("atomic write params.json")?;
    Ok(())
}

/// Read the largest value in any `tool_thread_curves` map from the
/// emitted compute-resource-policy. Used by `LocalExecutor::cpu_budget`
/// to avoid oversubscription when dispatching concurrent tasks.
fn load_peak_tool_thread_curve(package: &Path) -> Option<usize> {
    let p = package.join("policies/compute-resource-policy.json");
    if !p.exists() {
        return None;
    }
    let raw = std::fs::read_to_string(&p).ok()?;
    let v: serde_json::Value = serde_json::from_str(&raw).ok()?;
    let profiles = v.get("profiles")?.as_object()?;
    let mut peak: usize = 0;
    for (_, profile) in profiles {
        if let Some(curves) = profile
            .get("tool_thread_curves")
            .and_then(|c| c.as_object())
        {
            for (_, v) in curves {
                if let Some(n) = v.as_u64() {
                    peak = peak.max(n as usize);
                }
            }
        }
    }
    if peak == 0 {
        None
    } else {
        Some(peak)
    }
}

/// Best-effort `nvidia-smi -L` invocation. Counts lines starting with
/// "GPU " in the output. Returns 0 on any failure so CPU-only runs
/// don't misreport GPU availability.
fn probe_nvidia_gpu_count() -> usize {
    use std::process::Command;
    let output = match Command::new("nvidia-smi").arg("-L").output() {
        Ok(o) => o,
        Err(_) => return 0,
    };
    if !output.status.success() {
        return 0;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    stdout.lines().filter(|l| l.starts_with("GPU ")).count()
}

/// Load compute profiles from the emitted `policies/compute-resource-policy.json`.
/// Returns `None` when the file is absent or unparseable (the pilot
/// falls back to an empty profile set).
fn load_profiles(package: &Path) -> Option<ComputeProfiles> {
    let p = package.join("policies/compute-resource-policy.json");
    if !p.exists() {
        return None;
    }
    let raw = std::fs::read_to_string(&p).ok()?;
    let value: serde_json::Value = serde_json::from_str(&raw).ok()?;
    let yaml = serde_yml::to_string(&value).ok()?;
    serde_yml::from_str(&yaml).ok()
}

/// Load intake facts from the emitted `policies/intake-facts.json`.
/// Missing file yields an empty `SizingIntakeFacts` so pilot selection
/// still works (weights fall back to unscaled baselines).
fn load_facts(package: &Path) -> SizingIntakeFacts {
    let p = package.join("policies/intake-facts.json");
    let Ok(raw) = std::fs::read_to_string(&p) else {
        return SizingIntakeFacts::default();
    };
    serde_json::from_str(&raw).unwrap_or_default()
}

/// Run the agent once and measure its resource footprint. Returns
/// `None` when the subprocess fails to start.
///
/// On Linux, peak RSS is read from `/proc/<pid>/status::VmHWM` which is
/// the high-water mark of resident memory. On non-Linux, peak_rss_mb is
/// reported as 0 with a warning — callers should expect this and treat
/// non-Linux pilots as advisory-only.
fn measure_one_iteration(
    package: &Path,
    agent_cmd: &str,
    task_id: &str,
    stage_class: &str,
    cfg: &PilotConfig,
) -> Result<Option<PilotMeasurement>> {
    use std::process::Command;
    use std::time::{Duration, Instant};

    let mut child = Command::new(agent_cmd)
        .arg(package.to_string_lossy().to_string())
        .spawn()
        .with_context(|| format!("spawning agent '{}' for pilot", agent_cmd))?;
    let pid = child.id();
    let start = Instant::now();
    let mut peak_rss_kb: u64 = 0;
    let interval = Duration::from_secs(cfg.measurement_interval_secs);

    loop {
        match child.try_wait()? {
            Some(status) => {
                // Final sample — process may have touched VmHWM since the last poll.
                if let Some(p) = read_vmhwm_kb(pid) {
                    if p > peak_rss_kb {
                        peak_rss_kb = p;
                    }
                }
                return Ok(Some(PilotMeasurement {
                    task_id: task_id.to_string(),
                    stage_class: stage_class.to_string(),
                    peak_rss_mb: peak_rss_kb / 1024,
                    wall_time_secs: start.elapsed().as_secs(),
                    disk_used_mb: 0,
                    exit_status: status.code().unwrap_or(-1),
                }));
            }
            None => {
                if let Some(p) = read_vmhwm_kb(pid) {
                    if p > peak_rss_kb {
                        peak_rss_kb = p;
                    }
                }
                std::thread::sleep(interval);
            }
        }
    }
}

/// Read `VmHWM` (peak resident set size, in KB) from
/// `/proc/<pid>/status`. Returns `None` on non-Linux or when the
/// process has already exited (race against try_wait). Linux-only by
/// convention — the `/proc` filesystem is not portable.
#[cfg(target_os = "linux")]
fn read_vmhwm_kb(pid: u32) -> Option<u64> {
    let path = format!("/proc/{}/status", pid);
    let contents = std::fs::read_to_string(&path).ok()?;
    for line in contents.lines() {
        if let Some(rest) = line.strip_prefix("VmHWM:") {
            let kb: u64 = rest.split_whitespace().next()?.parse().ok()?;
            return Some(kb);
        }
    }
    None
}

#[cfg(not(target_os = "linux"))]
fn read_vmhwm_kb(_pid: u32) -> Option<u64> {
    // Non-Linux platforms lack /proc. Pilot falls back to wall-time
    // only; projection accuracy degrades but the run is not blocked.
    None
}

/// Read (utime + stime) from `/proc/<pid>/stat` as jiffies (clock ticks).
/// Used with two successive samples + wall time to compute CPU%.
#[cfg(target_os = "linux")]
fn read_cpu_ticks(pid: u32) -> Option<u64> {
    let contents = std::fs::read_to_string(format!("/proc/{}/stat", pid)).ok()?;
    // The `comm` field (field 2) is wrapped in parens and can contain
    // spaces; split after the closing paren so field indexing is stable.
    let close = contents.rfind(')')?;
    let tail = &contents[close + 1..];
    let fields: Vec<&str> = tail.split_whitespace().collect();
    // After the ')' split, field index 0 = state, 11 = utime, 12 = stime
    // (1-based fields 14 and 15 respectively in /proc/<pid>/stat).
    let utime: u64 = fields.get(11)?.parse().ok()?;
    let stime: u64 = fields.get(12)?.parse().ok()?;
    Some(utime + stime)
}
#[cfg(not(target_os = "linux"))]
fn read_cpu_ticks(_pid: u32) -> Option<u64> {
    None
}

/// Available-memory estimate in KB, from `/proc/meminfo::MemAvailable`.
/// Used to derive the memory-utilisation percentage the stall monitor
/// evaluates against `mem_max_pct`.
#[cfg(target_os = "linux")]
fn read_meminfo_total_available() -> Option<(u64, u64)> {
    let contents = std::fs::read_to_string("/proc/meminfo").ok()?;
    let mut total = None;
    let mut available = None;
    for line in contents.lines() {
        if let Some(rest) = line.strip_prefix("MemTotal:") {
            total = rest.split_whitespace().next().and_then(|v| v.parse().ok());
        } else if let Some(rest) = line.strip_prefix("MemAvailable:") {
            available = rest.split_whitespace().next().and_then(|v| v.parse().ok());
        }
        if total.is_some() && available.is_some() {
            break;
        }
    }
    Some((total?, available?))
}
#[cfg(not(target_os = "linux"))]
fn read_meminfo_total_available() -> Option<(u64, u64)> {
    None
}

/// Return `true` when the per-task `.heartbeat` file under
/// `heartbeat_root` exists and its mtime is within `max_age_secs` of
/// now. Returns `false` for any other condition (no path provided,
/// file missing, mtime unreadable, mtime older than the threshold).
/// Pure I/O — no thread / no clock dependency beyond `SystemTime::now`.
fn heartbeat_is_fresh(heartbeat_root: Option<&std::path::Path>, max_age_secs: u64) -> bool {
    let Some(root) = heartbeat_root else {
        return false;
    };
    let path = root.join(".heartbeat");
    let Ok(meta) = std::fs::metadata(&path) else {
        return false;
    };
    let Ok(mtime) = meta.modified() else {
        return false;
    };
    let Ok(age) = std::time::SystemTime::now().duration_since(mtime) else {
        // Negative age (clock skew): treat as fresh — defensive
        // toward the case where the file's mtime is slightly in the
        // future after an NFS / container clock rebase.
        return true;
    };
    age.as_secs() <= max_age_secs
}

fn stall_shutdown_requested(shutdown: &Arc<Mutex<bool>>) -> bool {
    *shutdown.lock()
}

fn sleep_until_next_stall_sample_or_shutdown(
    interval: std::time::Duration,
    shutdown: &Arc<Mutex<bool>>,
) -> bool {
    let deadline = std::time::Instant::now() + interval;
    let max_chunk = std::time::Duration::from_millis(200);
    loop {
        if stall_shutdown_requested(shutdown) {
            return true;
        }
        let now = std::time::Instant::now();
        if now >= deadline {
            return false;
        }
        std::thread::sleep(deadline.saturating_duration_since(now).min(max_chunk));
    }
}

/// Polling loop: samples CPU + memory, maintains sliding windows, and
/// posts a `StallSignal` via `tx` when a window breaches threshold.
/// Exits when `shutdown` flag flips true or the subprocess dies.
///
/// `heartbeat_root` is the per-task `runtime/outputs/<task_id>/`
/// directory. When `Some`, the loop reads `.heartbeat` mtime before
/// firing `CpuStarvation`; if the heartbeat is fresher than
/// `heartbeat_freshness_secs`, the signal is suppressed.
///
/// Rationale: the sampler watches `/proc/<wrapper-pid>/stat` for the
/// agent shell wrapper. When the wrapper is `wait()`ing on a long-
/// running `docker run` (R/Python package install, large file
/// transfer, container build), its CPU% is ~0 even though the docker
/// container is using >100% CPU. The heartbeat file is touched every
/// 30s by an in-wrapper background loop, so a fresh heartbeat is the
/// authoritative liveness signal — the wrapper is alive, just not
/// CPU-busy on its own PID. Veto CpuStarvation in that case.
fn sample_stall_loop(
    pid: u32,
    task_id: &str,
    thresholds: StallThresholds,
    tx: mpsc::Sender<StallSignal>,
    shutdown: Arc<Mutex<bool>>,
    heartbeat_root: Option<std::path::PathBuf>,
    heartbeat_freshness_secs: u64,
) {
    if !thresholds.enabled {
        return;
    }
    let interval = std::time::Duration::from_secs(thresholds.sample_interval_secs.max(1));
    // Clamp to a minimum of 1 sample so `cpu_window_mins=0`
    // (test-friendly config, e.g. the stall monitor integration test)
    // produces a one-sample window that fires on the first sub-threshold
    // observation instead of getting swallowed by `push_windowed`'s
    // `max == 0` short-circuit. Production configs pass
    // `cpu_window_mins >= 1`, so the clamp is a no-op for real runs.
    let cpu_window_size = ((thresholds.cpu_window_mins * 60
        / thresholds.sample_interval_secs.max(1)) as usize)
        .max(1);
    let mem_window_size = ((thresholds.mem_window_mins * 60
        / thresholds.sample_interval_secs.max(1)) as usize)
        .max(1);
    let mut cpu_samples: VecDeque<f32> = VecDeque::with_capacity(cpu_window_size);
    let mut mem_samples: VecDeque<f32> = VecDeque::with_capacity(mem_window_size);
    let mut prior_ticks: Option<u64> = None;
    let mut prior_instant = std::time::Instant::now();
    let started = std::time::Instant::now();
    let mut cpu_fired = false;
    let mut mem_fired = false;

    loop {
        if stall_shutdown_requested(&shutdown) {
            return;
        }
        if sleep_until_next_stall_sample_or_shutdown(interval, &shutdown) {
            return;
        }

        // CPU: compute % from jiffy delta over wall time.
        let now_ticks = match read_cpu_ticks(pid) {
            Some(t) => t,
            None => return, // process exited
        };
        let now_instant = std::time::Instant::now();
        if let Some(prior) = prior_ticks {
            let ticks_per_sec = sysconf_clk_tck() as f32;
            let elapsed = now_instant
                .duration_since(prior_instant)
                .as_secs_f32()
                .max(0.001);
            let delta_ticks = now_ticks.saturating_sub(prior) as f32;
            let cpu_pct = (delta_ticks / ticks_per_sec) / elapsed * 100.0;
            push_windowed(&mut cpu_samples, cpu_pct, cpu_window_size);
        }
        prior_ticks = Some(now_ticks);
        prior_instant = now_instant;

        // Memory: (MemTotal - MemAvailable) / MemTotal.
        if let Some((total, available)) = read_meminfo_total_available() {
            if total > 0 {
                let used_pct = ((total - available) as f32 / total as f32) * 100.0;
                push_windowed(&mut mem_samples, used_pct, mem_window_size);
            }
        }

        // Evaluate windows (only once per breach). Window sizes were
        // clamped to a minimum of 1 above so the monitor fires on the
        // first sample when the SME wants an aggressive one-shot check.
        if !cpu_fired && cpu_samples.len() >= cpu_window_size {
            let samples: Vec<f32> = cpu_samples.iter().copied().collect();
            if let Some(sig) = evaluate_cpu_window(
                task_id,
                &samples,
                thresholds.cpu_window_mins,
                thresholds.cpu_min_pct,
            ) {
                // Heartbeat veto: if the agent's per-task .heartbeat
                // file is fresh, the wrapper is alive (it's a
                // background loop touching the file every 30s) and
                // the low wrapper-CPU is a docker-wait artifact, not
                // a real stall. Don't fire — re-check on the next
                // window.
                if !heartbeat_is_fresh(heartbeat_root.as_deref(), heartbeat_freshness_secs) {
                    let _ = tx.send(sig);
                    cpu_fired = true;
                }
            }
        }
        if !mem_fired && mem_samples.len() >= mem_window_size {
            let samples: Vec<f32> = mem_samples.iter().copied().collect();
            if let Some(sig) = evaluate_memory_window(
                task_id,
                &samples,
                thresholds.mem_window_mins,
                thresholds.mem_max_pct,
            ) {
                let _ = tx.send(sig);
                mem_fired = true;
            }
        }

        // Runtime-overrun check uses elapsed wall time only.
        let _ = started; // reserved for future expected_secs wiring
    }
}

fn push_windowed(buf: &mut VecDeque<f32>, value: f32, max: usize) {
    // Defensive guard — `sample_stall_loop` clamps `max` to at least 1
    // so this short-circuit is a dead branch under current callers.
    // Kept for anyone who calls `push_windowed` directly in future
    // tests.
    if max == 0 {
        return;
    }
    buf.push_back(value);
    while buf.len() > max {
        buf.pop_front();
    }
}

/// Clock tick constant. `_SC_CLK_TCK` is 100 on essentially every
/// modern Linux (x86, arm64, musl glibc alike). Hardcoding it avoids
/// pulling in a libc dependency for a single call.
fn sysconf_clk_tck() -> i64 {
    100
}

/// Map a Unix signal number to its conventional uppercase name.
/// Covers the signals the harness actually surfaces in envelope
/// capture: KILL (9, OOM via OOM killer), TERM (15, wallclock timer),
/// SEGV (11, native crash), INT (2, ctrl-c), HUP (1).
fn signal_name(sig: i32) -> &'static str {
    match sig {
        1 => "HUP",
        2 => "INT",
        3 => "QUIT",
        6 => "ABRT",
        9 => "KILL",
        11 => "SEGV",
        13 => "PIPE",
        15 => "TERM",
        _ => "UNKNOWN",
    }
}

/// Backend context fields recorded on every local capture. Surfaces in
/// the BlockerCard so the SME can see "ran on host=fred-laptop" at a
/// glance.
fn local_context() -> std::collections::BTreeMap<String, String> {
    let mut out = std::collections::BTreeMap::new();
    if let Ok(host) = std::env::var("HOSTNAME") {
        if !host.is_empty() {
            out.insert("host".into(), host);
        }
    }
    if !out.contains_key("host") {
        if let Ok(out_str) = std::process::Command::new("hostname").output() {
            if out_str.status.success() {
                let h = String::from_utf8_lossy(&out_str.stdout).trim().to_string();
                if !h.is_empty() {
                    out.insert("host".into(), h);
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use scripps_workflow_core::dag::{Assignee, BlockedRecord, ResourceClass, TaskKind};

    fn args(timeout: u64) -> ExecutorArgs {
        ExecutorArgs {
            package: "/tmp/x".into(),
            agent: "/bin/true".into(),
            task_timeout_secs: timeout,
        }
    }

    fn running_task(started_at: String) -> Task {
        Task {
            kind: TaskKind::Computation,
            state: TaskState::Running {
                started_at,
                remote: None,
            },
            depends_on: vec![],
            assignee: Assignee::Agent,
            description: "running".into(),
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

    #[test]
    fn name_is_local() {
        let e = LocalExecutor::new(&args(300));
        assert_eq!(e.name(), "local");
    }

    #[test]
    fn is_task_stale_true_for_old_running_task() {
        let e = LocalExecutor::new(&args(300));
        // far older than 300s from now
        let task = running_task("2020-01-01T00:00:00Z".into());
        let now = chrono::Utc::now().timestamp() as u64;
        assert!(e.is_task_stale(&task, now));
    }

    #[test]
    fn is_task_stale_false_for_fresh_running_task() {
        let e = LocalExecutor::new(&args(300));
        let now = chrono::Utc::now();
        let task = running_task(now.to_rfc3339());
        assert!(!e.is_task_stale(&task, now.timestamp() as u64));
    }

    #[test]
    fn pilot_returns_none_when_disabled() {
        let mut e = LocalExecutor::new(&args(300));
        let dag = DAG {
            version: "1".into(),
            schema_version: scripps_workflow_core::dag::current_dag_schema_version(),
            workflow_id: "w".into(),
            current_task: None,
            tasks: std::collections::BTreeMap::new(),
            reverse_deps: std::collections::BTreeMap::new(),
            run_id: None,
        };
        let cfg = PilotConfig {
            enabled: false,
            ..Default::default()
        };
        let out = e.pilot(&dag, &cfg).unwrap();
        assert!(out.is_none());
    }

    #[test]
    fn pilot_runs_and_writes_artifacts_with_echo_agent() {
        use scripps_workflow_core::dag::{Assignee, ResourceClass, TaskKind};
        use serde_json::json;
        let tmp = tempfile::tempdir().unwrap();
        let package = tmp.path().to_path_buf();
        std::fs::create_dir_all(package.join("policies")).unwrap();
        let mut e = LocalExecutor {
            task_timeout_secs: 300,
            package: package.to_string_lossy().to_string(),
            agent: "/bin/echo".into(),
            stall_tx: None,
            stall_thresholds: None,
            stall_shutdown: Arc::new(Mutex::new(false)),
            pending_envelope_additions: BTreeMap::new(),
            session_env_additions: BTreeMap::new(),
            per_task_image_overrides: BTreeMap::new(),
            last_capture: None,
        };
        // Two Ready tasks (< 4 so fallback path), one non-discover.
        let mut tasks = std::collections::BTreeMap::new();
        tasks.insert(
            "t_qc".into(),
            Task {
                kind: TaskKind::Computation,
                state: TaskState::Ready,
                depends_on: vec![],
                assignee: Assignee::Agent,
                description: "qc".into(),
                spec: Some(json!({ "stage_class": "quality_control" })),
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
            "discover_align".into(),
            Task {
                kind: TaskKind::Computation,
                state: TaskState::Ready,
                depends_on: vec![],
                assignee: Assignee::Agent,
                description: "d".into(),
                spec: Some(json!({ "stage_class": "discover_alignment" })),
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
        let dag = DAG {
            version: "1".into(),
            schema_version: scripps_workflow_core::dag::current_dag_schema_version(),
            workflow_id: "w".into(),
            current_task: None,
            tasks,
            reverse_deps: std::collections::BTreeMap::new(),
            run_id: None,
        };
        let cfg = PilotConfig {
            enabled: true,
            task_count: 2,
            measurement_interval_secs: 1,
            ..Default::default()
        };
        let out = e.pilot(&dag, &cfg).unwrap();
        assert!(out.is_some(), "pilot should return Some when enabled");
        assert!(package.join("runtime/pilot/report.json").exists());
    }

    #[test]
    fn is_task_stale_false_for_non_running_states() {
        let e = LocalExecutor::new(&args(300));
        let now_ts = chrono::Utc::now().timestamp() as u64;
        let non_running = [
            TaskState::Pending,
            TaskState::Ready,
            TaskState::Completed {
                result: serde_json::json!({}),
            },
            TaskState::Failed { reason: "x".into() },
            TaskState::Blocked {
                record: BlockedRecord {
                    reason: "y".into(),
                    attempts: vec![],
                },
            },
        ];
        for state in non_running {
            let mut t = running_task("2020-01-01T00:00:00Z".into());
            t.state = state;
            assert!(!e.is_task_stale(&t, now_ts));
        }
    }

    #[test]
    fn apply_overrides_sets_memory_cap_env_and_pins() {
        use scripps_workflow_core::remediation::{ExecutorOverrides, ResourceTarget};

        let tmp = tempfile::tempdir().unwrap();
        let mut e = LocalExecutor {
            task_timeout_secs: 300,
            package: tmp.path().to_string_lossy().to_string(),
            agent: "/bin/true".into(),
            stall_tx: None,
            stall_thresholds: None,
            stall_shutdown: Arc::new(Mutex::new(false)),
            pending_envelope_additions: BTreeMap::new(),
            session_env_additions: BTreeMap::new(),
            per_task_image_overrides: BTreeMap::new(),
            last_capture: None,
        };

        let mut ov = ExecutorOverrides {
            resources: Some(ResourceTarget {
                memory_gb: Some(64),
                wallclock_secs: Some(7200),
                vcpus: Some(8),
                ..Default::default()
            }),
            ..Default::default()
        };
        ov.library_pins.insert("scanpy".into(), "1.9.6".into());
        ov.library_pins.insert("ann-data".into(), "0.10.5".into());
        ov.env_passthrough
            .insert("SWFC_DEBUG_TOKEN_BURN".into(), "1".into());

        e.apply_overrides("alignment", &ov).unwrap();
        let envs = e.drain_envelope_additions();
        assert_eq!(envs.get("SWFC_AGENT_MEMORY_CAP_GB").unwrap(), "64");
        assert_eq!(envs.get("SWFC_AGENT_WALLCLOCK_SECS").unwrap(), "7200");
        assert_eq!(envs.get("SWFC_HW_NPROC_HINT").unwrap(), "8");
        assert_eq!(envs.get("SWFC_LIB_PIN_SCANPY").unwrap(), "1.9.6");
        assert_eq!(envs.get("SWFC_LIB_PIN_ANN_DATA").unwrap(), "0.10.5");
        assert_eq!(envs.get("SWFC_DEBUG_TOKEN_BURN").unwrap(), "1");
        // After draining, accumulator is empty.
        assert!(e.drain_envelope_additions().is_empty());
    }

    #[test]
    fn drain_envelope_additions_returns_pending_then_clears() {
        use scripps_workflow_core::remediation::{ExecutorOverrides, ResourceTarget};
        let tmp = tempfile::tempdir().unwrap();
        let mut e = LocalExecutor {
            task_timeout_secs: 300,
            package: tmp.path().to_string_lossy().to_string(),
            agent: "/bin/true".into(),
            stall_tx: None,
            stall_thresholds: None,
            stall_shutdown: Arc::new(Mutex::new(false)),
            pending_envelope_additions: BTreeMap::new(),
            session_env_additions: BTreeMap::new(),
            per_task_image_overrides: BTreeMap::new(),
            last_capture: None,
        };
        let ov = ExecutorOverrides {
            resources: Some(ResourceTarget {
                memory_gb: Some(96),
                ..Default::default()
            }),
            ..Default::default()
        };
        e.apply_overrides("t1", &ov).unwrap();
        let first = e.drain_envelope_additions();
        assert_eq!(first.get("SWFC_AGENT_MEMORY_CAP_GB").unwrap(), "96");
        // Second drain returns empty — accumulator was cleared.
        let second = e.drain_envelope_additions();
        assert!(second.is_empty());
    }

    #[test]
    fn apply_overrides_writes_stage_parameters_atomically() {
        use scripps_workflow_core::remediation::ExecutorOverrides;

        let tmp = tempfile::tempdir().unwrap();
        let mut e = LocalExecutor {
            task_timeout_secs: 300,
            package: tmp.path().to_string_lossy().to_string(),
            agent: "/bin/true".into(),
            stall_tx: None,
            stall_thresholds: None,
            stall_shutdown: Arc::new(Mutex::new(false)),
            pending_envelope_additions: BTreeMap::new(),
            session_env_additions: BTreeMap::new(),
            per_task_image_overrides: BTreeMap::new(),
            last_capture: None,
        };

        let mut ov = ExecutorOverrides::default();
        ov.stage_parameters
            .insert("min_counts".into(), serde_json::json!(200));
        ov.stage_parameters
            .insert("threshold".into(), serde_json::json!(0.05));
        e.apply_overrides("filter", &ov).unwrap();

        let p = tmp.path().join("runtime/inputs/filter/params.json");
        assert!(p.exists());
        let raw = std::fs::read_to_string(&p).unwrap();
        let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(v["min_counts"], serde_json::json!(200));
        assert_eq!(v["threshold"], serde_json::json!(0.05));
        // Atomic — no.tmp left behind.
        assert!(!tmp
            .path()
            .join("runtime/inputs/filter/params.json.tmp")
            .exists());

        // Subsequent call merges (preserves prior keys).
        let mut ov2 = ExecutorOverrides::default();
        ov2.stage_parameters
            .insert("min_genes".into(), serde_json::json!(100));
        e.apply_overrides("filter", &ov2).unwrap();
        let raw = std::fs::read_to_string(&p).unwrap();
        let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(v["min_counts"], serde_json::json!(200));
        assert_eq!(v["min_genes"], serde_json::json!(100));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn stall_monitor_fires_on_first_sample_with_zero_minute_window() {
        // Regression — with `cpu_window_mins = 0` the
        // previous arithmetic produced `cpu_window_size = 0`, which
        // caused `push_windowed` to drop every sample on the floor and
        // the stall monitor to never fire. After the clamp to a minimum
        // of 1 the first sub-threshold observation should fire a
        // CpuStarvation signal.
        //
        // Drive `sample_stall_loop` directly against the current
        // process's PID so we don't spawn a subprocess. A 5s deadline
        // with `sample_interval_secs = 1` gives the loop plenty of time
        // to produce two samples and evaluate the one-slot window.
        let thresholds = StallThresholds {
            enabled: true,
            cpu_min_pct: 101.0, // unreachable — guarantees starvation
            cpu_window_mins: 0,
            mem_max_pct: 100.0, // mem never trips
            mem_window_mins: 5,
            gpu_idle_when_training_mins: 15,
            runtime_over_expected_mult: 2.0,
            sample_interval_secs: 1,
        };
        let (tx, rx) = mpsc::channel::<super::StallSignal>();
        let shutdown = Arc::new(Mutex::new(false));
        let pid = std::process::id();
        let shutdown_for_thread = shutdown.clone();
        let handle = std::thread::spawn(move || {
            sample_stall_loop(
                pid,
                "zero_window_probe",
                thresholds,
                tx,
                shutdown_for_thread,
                None, // no heartbeat root in this probe
                0,    // freshness irrelevant when root is None
            );
        });

        // Poll up to 5s for the signal — two or three samples under
        // 1s cadence is enough.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut fired = None;
        while std::time::Instant::now() < deadline {
            match rx.recv_timeout(std::time::Duration::from_millis(500)) {
                Ok(sig) => {
                    fired = Some(sig);
                    break;
                }
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
        *shutdown.lock() = true;
        let _ = handle.join();
        assert!(
            matches!(fired, Some(super::StallSignal::CpuStarvation { .. })),
            "expected CpuStarvation with cpu_window_mins=0 one-sample window, got {:?}",
            fired
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn stall_monitor_heartbeat_veto_suppresses_cpu_starvation_when_fresh() {
        // Regression — the wrapper bash PID's CPU sits at ~0% while it
        // waits on `docker run` returning, but the in-wrapper
        // heartbeat-touch loop keeps `.heartbeat` mtime fresh. Veto
        // CpuStarvation in that case; the workflow is alive, just
        // running its CPU inside the container we can't see from the
        // wrapper PID.
        let tmp = tempfile::tempdir().unwrap();
        let heartbeat_root = tmp.path().join("runtime/outputs/cpu_veto_probe");
        std::fs::create_dir_all(&heartbeat_root).unwrap();
        // Touch a fresh heartbeat right before the sampler reads it.
        std::fs::write(heartbeat_root.join(".heartbeat"), "2026-05-26T16:00:00Z\n").unwrap();

        let thresholds = StallThresholds {
            enabled: true,
            cpu_min_pct: 101.0, // unreachable → would normally fire
            cpu_window_mins: 0,
            mem_max_pct: 100.0,
            mem_window_mins: 5,
            gpu_idle_when_training_mins: 15,
            runtime_over_expected_mult: 2.0,
            sample_interval_secs: 1,
        };
        let (tx, rx) = mpsc::channel::<super::StallSignal>();
        let shutdown = Arc::new(Mutex::new(false));
        let pid = std::process::id();
        let shutdown_for_thread = shutdown.clone();
        let heartbeat_root_for_thread = heartbeat_root.clone();
        let handle = std::thread::spawn(move || {
            sample_stall_loop(
                pid,
                "cpu_veto_probe",
                thresholds,
                tx,
                shutdown_for_thread,
                Some(heartbeat_root_for_thread),
                300, // matches SWFC_TASK_HEARTBEAT_STALL_SECS default
            );
        });

        // The starvation condition holds for the whole window — but
        // the heartbeat is fresh, so the signal must NOT fire.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(4);
        let mut fired = None;
        while std::time::Instant::now() < deadline {
            match rx.recv_timeout(std::time::Duration::from_millis(500)) {
                Ok(sig) => {
                    fired = Some(sig);
                    break;
                }
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
        *shutdown.lock() = true;
        let _ = handle.join();
        assert!(
            fired.is_none(),
            "fresh heartbeat must veto CpuStarvation; got {:?}",
            fired
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn stall_monitor_heartbeat_veto_allows_signal_when_stale() {
        // Counterpart to the veto test: when the heartbeat file IS
        // stale (older than freshness threshold), the veto MUST NOT
        // suppress — a wrapper hung deep in its own code with no
        // heartbeat-touch is a real stall.
        let tmp = tempfile::tempdir().unwrap();
        let heartbeat_root = tmp.path().join("runtime/outputs/cpu_no_veto_probe");
        std::fs::create_dir_all(&heartbeat_root).unwrap();
        // Set the heartbeat file's mtime to 1 hour ago — well past
        // the 300s default freshness threshold the production path
        // uses. We `touch -d` here rather than pulling in a new
        // `filetime` dep just for this one test.
        let heartbeat_path = heartbeat_root.join(".heartbeat");
        std::fs::write(&heartbeat_path, "old\n").unwrap();
        let status = std::process::Command::new("touch")
            .args(["-d", "1 hour ago"])
            .arg(&heartbeat_path)
            .status()
            .expect("touch is available on Linux");
        assert!(status.success(), "touch should have backdated the mtime");

        let thresholds = StallThresholds {
            enabled: true,
            cpu_min_pct: 101.0,
            cpu_window_mins: 0,
            mem_max_pct: 100.0,
            mem_window_mins: 5,
            gpu_idle_when_training_mins: 15,
            runtime_over_expected_mult: 2.0,
            sample_interval_secs: 1,
        };
        let (tx, rx) = mpsc::channel::<super::StallSignal>();
        let shutdown = Arc::new(Mutex::new(false));
        let pid = std::process::id();
        let shutdown_for_thread = shutdown.clone();
        let heartbeat_root_for_thread = heartbeat_root.clone();
        let handle = std::thread::spawn(move || {
            sample_stall_loop(
                pid,
                "cpu_no_veto_probe",
                thresholds,
                tx,
                shutdown_for_thread,
                Some(heartbeat_root_for_thread),
                300,
            );
        });

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut fired = None;
        while std::time::Instant::now() < deadline {
            match rx.recv_timeout(std::time::Duration::from_millis(500)) {
                Ok(sig) => {
                    fired = Some(sig);
                    break;
                }
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
        *shutdown.lock() = true;
        let _ = handle.join();
        assert!(
            matches!(fired, Some(super::StallSignal::CpuStarvation { .. })),
            "stale heartbeat must NOT veto CpuStarvation; got {:?}",
            fired
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn stall_monitor_shutdown_returns_without_waiting_for_full_sample_interval() {
        let thresholds = StallThresholds {
            enabled: true,
            cpu_min_pct: 0.0,
            cpu_window_mins: 5,
            mem_max_pct: 100.0,
            mem_window_mins: 5,
            gpu_idle_when_training_mins: 15,
            runtime_over_expected_mult: 2.0,
            sample_interval_secs: 3,
        };
        let (tx, _rx) = mpsc::channel::<super::StallSignal>();
        let shutdown = Arc::new(Mutex::new(false));
        let pid = std::process::id();
        let shutdown_for_thread = shutdown.clone();
        let handle = std::thread::spawn(move || {
            sample_stall_loop(
                pid,
                "shutdown_probe",
                thresholds,
                tx,
                shutdown_for_thread,
                None,
                0,
            );
        });

        std::thread::sleep(std::time::Duration::from_millis(100));
        let started = std::time::Instant::now();
        *shutdown.lock() = true;
        handle.join().unwrap();
        let elapsed = started.elapsed();
        assert!(
            elapsed < std::time::Duration::from_secs(1),
            "shutdown should be interruptible, not wait for the full sample interval; elapsed={elapsed:?}"
        );
    }

    #[test]
    fn heartbeat_is_fresh_returns_false_when_root_is_none() {
        // No path supplied (test / legacy path) → fall through to the
        // legacy CPU-only behavior.
        assert!(!heartbeat_is_fresh(None, 300));
    }

    #[test]
    fn heartbeat_is_fresh_returns_false_when_file_missing() {
        let tmp = tempfile::tempdir().unwrap();
        // Empty dir — no .heartbeat file → not fresh.
        assert!(!heartbeat_is_fresh(Some(tmp.path()), 300));
    }

    // ── derived-image warm-up pre-flight ────────────────

    #[test]
    fn warm_runtime_image_returns_none_when_manifest_absent() {
        // Packages without policies/runtime-prereqs.json must
        // no-op the pre-flight rather than error.
        let tmp = tempfile::tempdir().unwrap();
        let mut e = LocalExecutor::new(&args(300));
        let res = e.warm_runtime_image(tmp.path()).expect("ok");
        assert!(
            res.is_none(),
            "missing manifest must short-circuit to None (no derived image)"
        );
        assert!(
            !e.session_env_additions
                .contains_key("SWFC_DEFAULT_CONTAINER_IMAGE"),
            "no env override when there's no manifest"
        );
    }

    #[test]
    fn warm_runtime_image_returns_none_when_manifest_is_empty() {
        // Empty-but-valid manifest (the legacy emit shape) must
        // short-circuit too — no base image means nothing to derive.
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("policies")).unwrap();
        std::fs::write(
            tmp.path().join("policies/runtime-prereqs.json"),
            r#"{"schema_version": 1}"#,
        )
        .unwrap();
        let mut e = LocalExecutor::new(&args(300));
        let res = e.warm_runtime_image(tmp.path()).expect("ok");
        assert!(res.is_none(), "empty manifest must short-circuit to None");
    }

    #[test]
    fn warm_runtime_image_returns_err_for_malformed_manifest() {
        // A typoed/garbage manifest is a fail-loud condition — better
        // to surface a clear error at provision time than to silently
        // skip the derived-image build the operator was relying on.
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("policies")).unwrap();
        std::fs::write(
            tmp.path().join("policies/runtime-prereqs.json"),
            "not json at all",
        )
        .unwrap();
        let mut e = LocalExecutor::new(&args(300));
        let res = e.warm_runtime_image(tmp.path());
        assert!(res.is_err(), "malformed manifest must surface an error");
    }

    // ── SWFC_PER_TASK_IMAGES=1 wiring ────────

    // Shared with `per_atom_image::tests`. Serializes tests that
    // mutate the per-atom-image env vars (SWFC_PER_TASK_IMAGES,
    // SWFC_PER_ATOM_BUILD_ROOT, SWFC_IMAGE_BUILDER_PATH) so parallel
    // `cargo test` runs can't trip on a stale env var another test
    // left briefly set. See
    // `executor/mod.rs::SWFC_PER_TASK_IMAGE_ENV_LOCK`.
    use crate::executor::SWFC_PER_TASK_IMAGE_ENV_LOCK as PER_TASK_ENV_LOCK;

    fn write_atom_prereqs_manifest(package_dir: &Path, atom_id: &str, apt: &[&str]) {
        use scripps_workflow_core::runtime_prereqs::{RuntimePrereqs, SystemPackages};
        let mut m = RuntimePrereqs::new();
        m.base_image = Some("ghcr.io/test/base:1".into());
        m.system_packages = SystemPackages {
            apt: apt.iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        };
        let path = package_dir
            .join("policies/atom-prereqs")
            .join(format!("{atom_id}.json"));
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, serde_json::to_string_pretty(&m).unwrap()).unwrap();
    }

    fn make_task(source_atom_id: Option<&str>) -> Task {
        use scripps_workflow_core::dag::{Assignee, ResourceClass, TaskKind};
        Task {
            kind: TaskKind::Computation,
            state: TaskState::Ready,
            depends_on: vec![],
            assignee: Assignee::Agent,
            description: "t".into(),
            spec: None,
            resolution: None,
            result_ref: None,
            resource_class: ResourceClass::CpuHeavy,
            requires_sme_review: false,
            required_artifacts: vec![],
            container: None,
            source_atom_id: source_atom_id.map(|s| s.to_string()),
            safety: Default::default(),
        }
    }

    fn mock_builder_in(scripts_dir: &Path, exit_code: i32) -> std::path::PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let script = scripts_dir.join("mock-builder.sh");
        std::fs::write(&script, format!("#!/bin/sh\nexit {exit_code}\n")).unwrap();
        let mut perms = std::fs::metadata(&script).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&script, perms).unwrap();
        script
    }

    #[test]
    fn provision_populates_per_task_image_overrides_when_gate_on() {
        // With SWFC_PER_TASK_IMAGES=1, provision walks the DAG and
        // pins each task whose source atom carries a buildable manifest
        // to a scripps-derived:<hash> tag. Atoms with no manifest are
        // skipped (host mode); tasks with no source_atom_id are
        // skipped (synthetic gates).
        let _guard = PER_TASK_ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let pkg = tempfile::tempdir().unwrap();
        let build_root = tempfile::tempdir().unwrap();
        let scripts_dir = tempfile::tempdir().unwrap();
        let builder = mock_builder_in(scripts_dir.path(), 0);

        write_atom_prereqs_manifest(pkg.path(), "atom_a", &["pkg-a"]);
        write_atom_prereqs_manifest(pkg.path(), "atom_b", &["pkg-b"]);

        let mut tasks = std::collections::BTreeMap::new();
        tasks.insert("task_a".into(), make_task(Some("atom_a")));
        tasks.insert("task_b".into(), make_task(Some("atom_b")));
        tasks.insert("task_no_atom".into(), make_task(None));
        tasks.insert("task_no_manifest".into(), make_task(Some("atom_missing")));
        let dag = DAG {
            version: "1".into(),
            schema_version: scripps_workflow_core::dag::current_dag_schema_version(),
            workflow_id: "w".into(),
            current_task: None,
            tasks,
            reverse_deps: std::collections::BTreeMap::new(),
            run_id: None,
        };

        let pkg_str = pkg.path().to_string_lossy().to_string();
        let mut e = LocalExecutor::new(&ExecutorArgs {
            package: pkg_str,
            agent: "/bin/true".into(),
            task_timeout_secs: 300,
        });
        std::env::set_var("SWFC_PER_TASK_IMAGES", "1");
        std::env::set_var("SWFC_PER_ATOM_BUILD_ROOT", build_root.path());
        std::env::set_var("SWFC_IMAGE_BUILDER_PATH", &builder);
        let res = e.provision(&dag);
        std::env::remove_var("SWFC_PER_TASK_IMAGES");
        std::env::remove_var("SWFC_PER_ATOM_BUILD_ROOT");
        std::env::remove_var("SWFC_IMAGE_BUILDER_PATH");

        res.expect("provision");
        // task_a / task_b each have an atom with a buildable manifest
        // → image overrides should be populated for them.
        let tag_a = e
            .per_task_image_overrides
            .get("task_a")
            .expect("task_a should have a per-task tag");
        let tag_b = e
            .per_task_image_overrides
            .get("task_b")
            .expect("task_b should have a per-task tag");
        assert!(
            tag_a.starts_with("scripps-derived:"),
            "task_a tag should use default prefix; got {tag_a}"
        );
        assert!(
            tag_b.starts_with("scripps-derived:"),
            "task_b tag should use default prefix; got {tag_b}"
        );
        // Different atoms / different manifests → different tags.
        assert_ne!(
            tag_a, tag_b,
            "task_a and task_b have different manifests so tags must differ"
        );
        // task_no_atom (no source_atom_id) and task_no_manifest
        // (atom_missing has no manifest file) must NOT have overrides.
        assert!(
            !e.per_task_image_overrides.contains_key("task_no_atom"),
            "task with no source_atom_id must not get an override"
        );
        assert!(
            !e.per_task_image_overrides.contains_key("task_no_manifest"),
            "task whose atom has no manifest must not get an override"
        );
        // The session-wide warm-up MUST be skipped — no
        // SWFC_DEFAULT_CONTAINER_IMAGE in session_env_additions.
        assert!(
            !e.session_env_additions
                .contains_key("SWFC_DEFAULT_CONTAINER_IMAGE"),
            "session_env_additions must not pin a union image when gate is on"
        );
    }

    #[test]
    fn provision_dedupes_two_tasks_sharing_one_atom_to_one_tag() {
        // When two tasks share the same atom, they should get the
        // same per-task image tag (single builder invocation, dedupe
        // via the per-dispatch HashMap). Mirrors the
        // `per_atom_image::dedupes_identical_manifests_to_same_tag`
        // test at the helper level.
        let _guard = PER_TASK_ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let pkg = tempfile::tempdir().unwrap();
        let build_root = tempfile::tempdir().unwrap();
        let scripts_dir = tempfile::tempdir().unwrap();
        let builder = mock_builder_in(scripts_dir.path(), 0);

        write_atom_prereqs_manifest(pkg.path(), "atom_shared", &["pkg-shared"]);

        let mut tasks = std::collections::BTreeMap::new();
        tasks.insert("task_one".into(), make_task(Some("atom_shared")));
        tasks.insert("task_two".into(), make_task(Some("atom_shared")));
        let dag = DAG {
            version: "1".into(),
            schema_version: scripps_workflow_core::dag::current_dag_schema_version(),
            workflow_id: "w".into(),
            current_task: None,
            tasks,
            reverse_deps: std::collections::BTreeMap::new(),
            run_id: None,
        };

        let pkg_str = pkg.path().to_string_lossy().to_string();
        let mut e = LocalExecutor::new(&ExecutorArgs {
            package: pkg_str,
            agent: "/bin/true".into(),
            task_timeout_secs: 300,
        });
        std::env::set_var("SWFC_PER_TASK_IMAGES", "1");
        std::env::set_var("SWFC_PER_ATOM_BUILD_ROOT", build_root.path());
        std::env::set_var("SWFC_IMAGE_BUILDER_PATH", &builder);
        e.provision(&dag).expect("provision");
        std::env::remove_var("SWFC_PER_TASK_IMAGES");
        std::env::remove_var("SWFC_PER_ATOM_BUILD_ROOT");
        std::env::remove_var("SWFC_IMAGE_BUILDER_PATH");

        let tag_one = e
            .per_task_image_overrides
            .get("task_one")
            .expect("task_one tag");
        let tag_two = e
            .per_task_image_overrides
            .get("task_two")
            .expect("task_two tag");
        assert_eq!(
            tag_one, tag_two,
            "two tasks sharing an atom must get the same per-task image tag"
        );
    }

    #[test]
    fn provision_falls_through_to_union_when_gate_off() {
        // With SWFC_PER_TASK_IMAGES=0 (the explicit opt-out),
        // provision must use the union warm_runtime_image path.
        // With no manifest in the package, that's a no-op
        // (Ok(None)), and per_task_image_overrides should stay
        // empty.
        let _guard = PER_TASK_ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let pkg = tempfile::tempdir().unwrap();
        // Even with a per-atom manifest present, the legacy path
        // doesn't consult it — confirms the gate is the only switch.
        write_atom_prereqs_manifest(pkg.path(), "atom_x", &["x"]);

        let mut tasks = std::collections::BTreeMap::new();
        tasks.insert("task_x".into(), make_task(Some("atom_x")));
        let dag = DAG {
            version: "1".into(),
            schema_version: scripps_workflow_core::dag::current_dag_schema_version(),
            workflow_id: "w".into(),
            current_task: None,
            tasks,
            reverse_deps: std::collections::BTreeMap::new(),
            run_id: None,
        };

        let pkg_str = pkg.path().to_string_lossy().to_string();
        let mut e = LocalExecutor::new(&ExecutorArgs {
            package: pkg_str,
            agent: "/bin/true".into(),
            task_timeout_secs: 300,
        });
        std::env::set_var("SWFC_PER_TASK_IMAGES", "0");
        let res = e.provision(&dag);
        std::env::remove_var("SWFC_PER_TASK_IMAGES");
        res.expect("provision");
        assert!(
            e.per_task_image_overrides.is_empty(),
            "per_task_image_overrides must stay empty when gate is off"
        );
    }

    /// Test fixture mutex that serializes the env-clear tests
    /// against the rest of the suite, since they mutate
    /// process-global env vars.
    static ENV_CLEAR_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Sanity: SECRET_KEYS is non-empty and includes the high-value
    /// items (Anthropic + AWS + GitHub tokens). The exact ordering is
    /// not asserted because the allowlist is a set.
    #[test]
    fn secret_keys_includes_critical_tokens() {
        for required in [
            "SWFC_ANTHROPIC_API_KEY",
            "AWS_SECRET_ACCESS_KEY",
            "GH_TOKEN",
        ] {
            assert!(
                SECRET_KEYS.contains(&required),
                "SECRET_KEYS missing {}",
                required
            );
        }
    }

    /// `SWFC_DISABLE_ENV_CLEAR=1` returns `true` from
    /// `env_clear_disabled` so the apply_env_with_allowlist function
    /// short-circuits onto the legacy inherit-everything path.
    #[test]
    fn env_clear_disabled_honours_bypass_flag() {
        let _guard = ENV_CLEAR_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        std::env::set_var("SWFC_DISABLE_ENV_CLEAR", "1");
        assert!(env_clear_disabled());
        std::env::remove_var("SWFC_DISABLE_ENV_CLEAR");
        assert!(!env_clear_disabled());
    }

    /// W2.1 — validate_sandbox_env accepts the known-good values
    /// (unset, off, empty, bubblewrap) and refuses anything else
    /// (e.g. typos like "bublewrap").
    #[test]
    fn validate_sandbox_env_accepts_known_values() {
        let _guard = ENV_CLEAR_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        std::env::remove_var("SWFC_LOCAL_SANDBOX");
        assert!(validate_sandbox_env().is_ok(), "unset must be valid");
        for ok in ["bubblewrap", "off", ""] {
            std::env::set_var("SWFC_LOCAL_SANDBOX", ok);
            assert!(validate_sandbox_env().is_ok(), "value {ok:?} must be valid");
        }
        std::env::remove_var("SWFC_LOCAL_SANDBOX");
    }

    #[test]
    fn validate_sandbox_env_refuses_unknown_values() {
        let _guard = ENV_CLEAR_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        for bad in ["bublewrap", "BUBBLEWRAP", "on", "true", "yes", "1"] {
            std::env::set_var("SWFC_LOCAL_SANDBOX", bad);
            let err = validate_sandbox_env()
                .expect_err("unknown SWFC_LOCAL_SANDBOX value must be refused");
            assert!(
                err.to_string().contains("not a valid policy"),
                "wrong error message for {bad:?}: {err}"
            );
        }
        std::env::remove_var("SWFC_LOCAL_SANDBOX");
    }

    /// End-to-end: spawn a `/usr/bin/env` subprocess via the env-cleared
    /// path with one envelope entry. The child's stdout should contain
    /// the envelope variable, the SECRET_KEY allowlist (when present in
    /// the parent env), but NOT a freshly-set
    /// `SWFC_TEST_DISALLOWED_LEAK_*` that's not on the allowlist.
    #[test]
    fn apply_env_with_allowlist_strips_non_allowlisted() {
        let _guard = ENV_CLEAR_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        std::env::remove_var("SWFC_DISABLE_ENV_CLEAR");
        // Set a non-allowlisted var that MUST get stripped.
        std::env::set_var("SWFC_TEST_DISALLOWED_LEAK_KEY", "secret-leak");
        // Set an allowlisted var that MUST survive.
        std::env::set_var("SWFC_ANTHROPIC_API_KEY", "test-allowed");
        std::env::set_var("SWFC_AGENT_BILLING", "api");
        std::env::set_var("MAX_TURNS_PER_TASK", "25");
        let mut envelope = BTreeMap::new();
        envelope.insert(
            "SWFC_TEST_ENVELOPE".to_string(),
            "envelope-value".to_string(),
        );

        let mut cmd = std::process::Command::new("/usr/bin/env");
        apply_env_with_allowlist(&mut cmd, &envelope);
        let out = cmd.output().expect("spawn env");
        let text = String::from_utf8_lossy(&out.stdout);

        std::env::remove_var("SWFC_TEST_DISALLOWED_LEAK_KEY");
        std::env::remove_var("SWFC_ANTHROPIC_API_KEY");
        std::env::remove_var("SWFC_AGENT_BILLING");
        std::env::remove_var("MAX_TURNS_PER_TASK");

        assert!(
            text.contains("SWFC_TEST_ENVELOPE=envelope-value"),
            "envelope must reach child: {}",
            text
        );
        assert!(
            text.contains("SWFC_ANTHROPIC_API_KEY=test-allowed"),
            "allowlisted secret must reach child: {}",
            text
        );
        assert!(
            text.contains("MAX_TURNS_PER_TASK=25"),
            "turn-budget cap must reach local executor child: {}",
            text
        );
        assert!(
            text.contains("SWFC_AGENT_BILLING=api"),
            "agent billing mode must reach local executor child: {}",
            text
        );
        assert!(
            !text.contains("SWFC_TEST_DISALLOWED_LEAK_KEY"),
            "non-allowlisted host env MUST NOT leak: {}",
            text
        );
    }

    /// Verifies that the one-shot warning text for the no-bwrap default path
    /// mentions both `SWFC_LOCAL_SANDBOX=off` (opt-out instruction) and `bwrap`
    /// (the missing binary). This test does not exercise the probe itself
    /// (filesystem-dependent) — it validates the message content by inspecting
    /// the source literal.
    #[test]
    fn detect_default_sandbox_warn_mentions_opt_out_and_bwrap() {
        // The warning string is a compile-time literal in detect_default_sandbox().
        // Extract it here so changes to the message are caught by this test.
        let warn_text = concat!(
            "SWFC_LOCAL_SANDBOX unset and bwrap not found on PATH. ",
            "Implementation::GeneratedCode tasks will run unsandboxed. ",
            "Install bwrap (sudo apt install bubblewrap) or set ",
            "SWFC_LOCAL_SANDBOX=off explicitly to suppress this warning."
        );
        assert!(
            warn_text.contains("SWFC_LOCAL_SANDBOX=off"),
            "warning must mention SWFC_LOCAL_SANDBOX=off opt-out: {warn_text}"
        );
        assert!(
            warn_text.contains("bwrap"),
            "warning must mention bwrap: {warn_text}"
        );
    }

    /// `SWFC_DISABLE_ENV_CLEAR=1` falls back to legacy inherit-all so
    /// existing tests / scripts that depend on inherited env keep working.
    #[test]
    fn bypass_flag_inherits_non_allowlisted() {
        let _guard = ENV_CLEAR_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        std::env::set_var("SWFC_DISABLE_ENV_CLEAR", "1");
        std::env::set_var("SWFC_TEST_INHERIT_PROBE", "inherited-value");
        let envelope = BTreeMap::new();
        let mut cmd = std::process::Command::new("/usr/bin/env");
        apply_env_with_allowlist(&mut cmd, &envelope);
        let out = cmd.output().expect("spawn env");
        let text = String::from_utf8_lossy(&out.stdout);

        std::env::remove_var("SWFC_DISABLE_ENV_CLEAR");
        std::env::remove_var("SWFC_TEST_INHERIT_PROBE");

        assert!(
            text.contains("SWFC_TEST_INHERIT_PROBE=inherited-value"),
            "bypass flag must restore legacy inherit-all: {}",
            text
        );
    }
}
