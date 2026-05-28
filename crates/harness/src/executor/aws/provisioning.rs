//! Instance provisioning, lifecycle, and the pilot driver. All paths
//! that shell out to `aws ec2` (run-instances / describe-instances /
//! terminate-instances) live here, plus the `aws ssm describe-instance-information`
//! readiness wait.

use super::super::cost_guard::PricingSource;
use super::super::pilot::{PilotConfig, PilotMeasurement, PilotReport};
use super::super::sizing::{compute_high_water, merge_resource_requirements_max, ComputeProfiles};
use super::sizing::{load_aws_facts, load_aws_profiles, task_stage_class};
use super::{AwsExecutor, ProvisionedInstance};
use anyhow::{anyhow, Context, Result};
use scripps_workflow_core::dag::DAG;
use scripps_workflow_core::remediation::ResourceTarget;
use std::path::Path;
use std::sync::Once;
use std::time::Duration;

/// Parse the `SWFC_AWS_INSTANCE_TYPE_ALLOWLIST`
/// env var into a vector of trimmed instance-type names. Unset (or
/// empty / whitespace) returns `None` so callers can apply
/// "unconfigured = allow any" semantics intentionally.
fn parse_instance_type_allowlist() -> Option<Vec<String>> {
    let raw = std::env::var("SWFC_AWS_INSTANCE_TYPE_ALLOWLIST").ok()?;
    let items: Vec<String> = raw
        .split(',')
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
        .collect();
    if items.is_empty() {
        None
    } else {
        Some(items)
    }
}

static ALLOWLIST_UNSET_WARN: Once = Once::new();

/// Reject any instance type the operator hasn't
/// added to `SWFC_AWS_INSTANCE_TYPE_ALLOWLIST`. Unconfigured ⇒ allow
/// any type (preserves the historical default while still letting an
/// operator opt-in to a narrower surface).
pub(super) fn check_instance_type_allowed(instance_type: &str) -> Result<()> {
    match parse_instance_type_allowlist() {
        None => {
            // Surface a single startup warning so operators on a fresh
            // install discover the allowlist exists. We log once per
            // process (Once) to keep the loop quiet.
            ALLOWLIST_UNSET_WARN.call_once(|| {
                tracing::warn!(
                    instance_type = instance_type,
                    "SWFC_AWS_INSTANCE_TYPE_ALLOWLIST is unset; any instance type \
                     the sizing layer picks will be launched. Set the env var to \
                     a comma-separated list (e.g. \
                     t3.medium,m6i.large,c6i.large,r6i.large,r6i.xlarge,g5.xlarge) \
                     to restrict the harness's blast radius."
                );
            });
            Ok(())
        }
        Some(list) if list.iter().any(|t| t == instance_type) => Ok(()),
        Some(list) => Err(anyhow!(
            "instance type {} not in SWFC_AWS_INSTANCE_TYPE_ALLOWLIST {:?}; \
             widen the allowlist or change the sizing policy.",
            instance_type,
            list
        )),
    }
}

/// Compose a `--client-token` for `ec2 run-instances` so a
/// transient network error mid-API-call can't double-launch
/// within one harness process. AWS guarantees
/// idempotency on the token for ~1 hour: a duplicate call returns
/// the original instance instead of creating a new one.
///
/// Shape: `<run_id>-<subnet>` truncated to 64 chars (AWS hard limit).
/// `run_id` is the AwsExecutor-scoped UUID generated at construction,
/// stable across all provisioning attempts in a process. Per-subnet
/// suffix means each subnet rotation gets its own dedup namespace
/// (different request, different token, so AWS doesn't reject the
/// second subnet as an idempotency violation).
///
/// Caveat: if the harness process crashes after the API call but
/// before we record the instance id, the restarted process
/// generates a fresh `run_id` and AWS sees a new request →
/// duplicate launch. `load_or_create_run_id` (below) closes that
/// gap by persisting the run_id to a package-scoped sidecar file,
/// so a crash-restart reuses the same id and AWS idempotency keeps
/// working.
pub(super) fn compose_client_token(run_id: &str, subnet_id: &str) -> String {
    let raw = format!("{}-{}", run_id, subnet_id);
    if raw.len() <= 64 {
        raw
    } else {
        // Hash-truncate so the token stays unique. Simple FNV-1a over
        // the original is sufficient — AWS doesn't read the contents,
        // it just compares for equality. We keep a human-readable
        // prefix so logs are diagnosable.
        let mut h: u64 = 0xcbf29ce484222325;
        for b in raw.as_bytes() {
            h ^= *b as u64;
            h = h.wrapping_mul(0x100000001b3);
        }
        let prefix: String = raw.chars().take(45).collect();
        format!("{}-{:016x}", prefix, h)
    }
}

/// Load the persisted `run_id` for this package,
/// or generate + persist a fresh one. Sidecar file at
/// `<pkg>/runtime/.harness-run-id`. Reusing the same id across
/// crash-restarts keeps `compose_client_token` idempotency working
/// (AWS sees the retried `run-instances` as a duplicate and returns
/// the original instance instead of double-launching).
///
/// Best-effort: a write failure (read-only fs, permission error)
/// degrades to "fresh UUID every call" — same behavior as before
/// And the next provisioning attempt may double-launch
/// only if it falls inside the AWS idempotency window AND the prior
/// call's instance is unreachable. The orphan-reap is the
/// backstop.
pub(super) fn load_or_create_run_id(pkg: &Path) -> String {
    let path = pkg.join("runtime/.harness-run-id");
    if let Ok(s) = std::fs::read_to_string(&path) {
        let trimmed = s.trim();
        // Accept any well-formed 32-char hex string (the existing
        // format from `Uuid::simple()`). Reject anything else and
        // regenerate, so a corrupted sidecar can't permanently brick
        // provisioning.
        if trimmed.len() == 32 && trimmed.chars().all(|c| c.is_ascii_hexdigit()) {
            return trimmed.to_string();
        }
        tracing::warn!(
            path = %path.display(),
            "discarding malformed run_id sidecar (length {} chars) and regenerating",
            trimmed.len()
        );
    }
    let fresh = uuid::Uuid::new_v4().simple().to_string();
    // Best-effort persist. We deliberately ignore errors here — a
    // sidecar write failure is recoverable (we still get the
    // idempotency in-process), but a panic at startup would be much
    // worse.
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Err(e) = std::fs::write(&path, &fresh) {
        tracing::warn!(
            path = %path.display(),
            error = %e,
            "could not persist run_id sidecar; crash-restart will draw fresh id"
        );
    }
    fresh
}

/// Apply a remediation `ResourceTarget` as a floor over the
/// profile-driven `ResourceRequirements`. Each override field raises
/// the corresponding requirement when set; absent fields are left
/// unchanged.
fn apply_resource_target_min(
    req: &mut super::super::ResourceRequirements,
    target: &ResourceTarget,
) {
    if let Some(v) = target.vcpus {
        req.vcpus = req.vcpus.max(v);
    }
    if let Some(v) = target.memory_gb {
        req.memory_gb = req.memory_gb.max(v);
    }
    if let Some(v) = target.storage_gb {
        req.storage_gb = req.storage_gb.max(v);
    }
    if let Some(g) = target.gpu.as_ref() {
        req.gpu = Some(super::super::GpuRequirement {
            kind: g.kind.clone(),
            count: g.count,
        });
    }
}

// `scan_orphans` + `wait_for_ssm` are public helpers that live on
// AwsExecutor but aren't called from the `Executor` trait dispatch.
// They're defined in `mod.rs` alongside the other public inspectors
// (`instance_id`, `instance_type`, `remote_execution_info`) — Rust
// allows inherent impls to span modules, so `self.wait_for_ssm(...)`
// still resolves inside this file.

impl AwsExecutor {
    /// Resolve the instance type for the package's first stage. We
    /// walk the DAG per-task and pick a representative shape so we can
    /// launch one instance.
    ///
    /// When `pilot_instance_override` is set, return it unchanged —
    /// the pilot uses this to force a small dedicated shape (e.g.
    /// `t3.medium`) regardless of the DAG's real sizing profile. The
    /// override is cleared by `pilot` after the pilot instance is
    /// released, so the main loop's subsequent `provision` call reverts
    /// to profile-driven sizing.
    pub(super) fn pick_instance_type(&self, dag: &DAG) -> Result<String> {
        if let Some(override_type) = self.pilot_instance_override.as_ref() {
            return Ok(override_type.clone());
        }
        let package = std::path::Path::new(&self.args.package);
        let profiles_path = package.join("policies/compute-resource-policy.json");
        let profiles = if profiles_path.exists() {
            let raw = std::fs::read_to_string(&profiles_path)
                .with_context(|| format!("reading {}", profiles_path.display()))?;
            // The emitter writes profiles as JSON converted from YAML.
            // Parse via serde_json then re-encode as YAML so the sizing
            // loader (YAML-only) consumes it.
            let v: serde_json::Value = serde_json::from_str(&raw)?;
            let yaml = serde_yml::to_string(&v)?;
            Some(serde_yml::from_str::<ComputeProfiles>(&yaml)?)
        } else {
            None
        };
        let facts = load_aws_facts(package);

        // Size the single EC2 instance to cover the heaviest real
        // stage in the DAG. Pilot projections, when present, raise the
        // static profile floor; static method/profile requirements are
        // still merged in so pilot output cannot drop GPU/storage needs.
        let mut req: Option<super::super::ResourceRequirements> = None;
        for task in dag.tasks.values() {
            let stage_class = task_stage_class(task);
            if stage_class.is_empty() {
                continue;
            }
            let baseline = profiles
                .as_ref()
                .and_then(|p| compute_high_water(p, &stage_class, &facts, &[]));
            let projected = self
                .pilot_projected_requirements
                .as_ref()
                .and_then(|m| m.get(&stage_class))
                .cloned();
            let stage_req = match (baseline, projected) {
                (Some(base), Some(proj)) => Some(merge_resource_requirements_max(&base, &proj)),
                (Some(base), None) => Some(base),
                (None, Some(proj)) => Some(proj),
                (None, None) => None,
            };
            if let Some(stage_req) = stage_req {
                req = Some(match req {
                    Some(existing) => merge_resource_requirements_max(&existing, &stage_req),
                    None => stage_req,
                });
            }
        }

        let mut req = req.unwrap_or(super::super::ResourceRequirements {
            // Review-only or policy-less DAG — pick the burstable default.
            vcpus: 2,
            memory_gb: 4,
            storage_gb: 50,
            gpu: None,
        });
        if let Some(target) = self.pending_resources_override.as_ref() {
            apply_resource_target_min(&mut req, target);
        }
        Ok(super::super::sizing::resolve_instance_type(&req))
    }

    pub(super) fn do_provision(&mut self, dag: &DAG) -> Result<()> {
        // Build the internal ProgressClient if a session endpoint was wired
        // in via `set_session_endpoint` but the client hasn't been constructed
        // yet. Idempotent — no-op on subsequent calls.
        self.ensure_progress_client();
        // Cost guard: estimate the planned spend before launching.
        // R2-N21 — was `self.cost_model.estimate_run_cost_usd(...)` /
        // `self.cost_model.check_ceiling(...)` through the deleted
        // `CostModel` trait; now dispatches via `BackendKind` against
        // the free functions in `cost_guard.rs`. Behavior preserved:
        // `BackendKind::Aws` routes to `estimate_run_cost_usd` +
        // `check_ceiling` with the same env-var ceiling semantics.
        let instance_type = self.pick_instance_type(dag)?;
        // Instance-type allowlist guard. Runs
        // before the cost guard so an operator with a tight allowlist
        // gets a clear "not allowed" diagnostic before we waste a
        // pricing-table lookup. Unconfigured allowlist degrades to a
        // permissive default (with a one-shot warn).
        check_instance_type_allowed(&instance_type)?;
        let projected_hours = (self.args.task_timeout_secs as f64) / 3600.0;
        let pricing_source = if self.config.spot {
            PricingSource::Spot
        } else {
            PricingSource::OnDemand
        };
        let estimate = super::super::cost_guard::estimate_for_backend(
            self.cost_backend,
            &[(instance_type.clone(), projected_hours)],
            pricing_source,
        )?;
        super::super::cost_guard::check_ceiling_for_backend(self.cost_backend, estimate)?;

        // R-21 — cumulative-spend tracker. The per-launch ceiling
        // catches a single oversized provision; the cumulative tracker
        // catches a sequence of small provisions (spot reclaim loops,
        // capacity rebalance, manual retries) that piecewise stay
        // below the per-launch ceiling but in aggregate breach the
        // run-total budget. W5.1: use the fail-closed constructor so a
        // missing SWFC_AWS_RUN_TOTAL_CEILING_USD aborts provisioning
        // instead of silently applying the $100 default.
        let cumulative =
            super::super::cost_guard::CumulativeSpend::for_package_strict(&self.args.package)?;
        if let Some(next_estimate) = estimate {
            cumulative.check_cumulative(next_estimate)?;
            // Record before launch so a launch that succeeds against
            // AWS but errors during parsing still counts toward the
            // running total. The cumulative budget is "AWS has issued
            // us this much spend", not "we've successfully parsed
            // back this much spend".
            let new_cumulative = cumulative
                .record_provision(next_estimate)
                .map_err(anyhow::Error::from)?;

            // Emit a `cost_guard_passed` progress event so operators
            // see cumulative-spend status on every successful check,
            // not only on abort-on-overage. The event is a no-op when
            // the harness runs without a `--session-id`.
            if let Some(ref pc) = self.progress_client {
                let ceiling_usd =
                    super::super::cost_guard::read_provision_ceiling_usd().unwrap_or(0.0);
                super::super::cost_guard::emit_cost_guard_passed(
                    pc,
                    "",
                    next_estimate,
                    ceiling_usd,
                    new_cumulative,
                    cumulative.ceiling_usd(),
                );
            }
        }

        // Pick the security group from `task.safety.network` floor across
        // the DAG. `required_network_floor` returns
        // `NetworkPolicy::None` when ANY Network/Exec task asks for
        // egress-restricted; otherwise `Bridge`. When the floor is
        // restricted but no `SWFC_AWS_RESTRICTED_SG_ID` is configured,
        // we fall back to the permissive SG and let
        // `enforce_safety_policy` (running against the capability we
        // advertise) block the offending task with
        // `BlockerKind::NetworkPolicyMismatch` — that surfaces a typed
        // diagnostic to the SME instead of silently provisioning under
        // a permissive SG.
        let net_floor = super::required_network_floor(dag);
        let (sg_id, effective_net) = match (&net_floor, &self.config.restricted_security_group) {
            (scripps_workflow_core::atom::NetworkPolicy::None { .. }, Some(restricted_sg)) => {
                (restricted_sg.clone(), net_floor.clone())
            }
            (scripps_workflow_core::atom::NetworkPolicy::None { .. }, None) => {
                tracing::warn!(
                    "SWFC_AWS_RESTRICTED_SG_ID is unset but DAG carries a task with \
                     safety.network=None; falling back to permissive SG. Affected tasks \
                     will be Blocked with NetworkPolicyMismatch."
                );
                (
                    self.config.security_group.clone(),
                    scripps_workflow_core::atom::NetworkPolicy::Bridge,
                )
            }
            _ => (
                self.config.security_group.clone(),
                scripps_workflow_core::atom::NetworkPolicy::Bridge,
            ),
        };
        if let Ok(mut g) = self.effective_network.lock() {
            *g = Some(effective_net);
        }

        // Multi-AZ rotation: try each configured subnet in order.
        let mut subnets = self.config.subnets.clone();
        let mut last_error: Option<anyhow::Error> = None;
        let mut launched: Option<(String, String)> = None;

        while let Some(subnet_id) = subnets.advance() {
            let mut args: Vec<String> = vec![
                "ec2".into(),
                "run-instances".into(),
                "--image-id".into(),
                self.config.ami_id.clone(),
                "--instance-type".into(),
                instance_type.clone(),
                "--subnet-id".into(),
                subnet_id.clone(),
                "--security-group-ids".into(),
                sg_id.clone(),
                "--iam-instance-profile".into(),
                format!("Name={}", self.config.instance_profile),
                "--tag-specifications".into(),
                {
                    // session-id tag namespacing so
                    // session A's orphan sweep cannot reap session B's
                    // live instances. Tag is absent when the harness
                    // was launched without `--session-id` (e.g. CI
                    // smoke runs); those paths fall back to the legacy
                    // BuiltBy-only filter.
                    let sid_tag = self
                        .config
                        .harness_session_id
                        .as_deref()
                        .map(|s| format!(",{{Key=ScrippsWorkflowHarnessSessionId,Value={}}}", s))
                        .unwrap_or_default();
                    format!(
                        "ResourceType=instance,Tags=[{{Key=BuiltBy,Value=scripps-workflow-harness}},{{Key=WorkspaceSha,Value={}}}{}]",
                        self.config.workspace_sha, sid_tag
                    )
                },
                "--count".into(),
                "1".into(),
                "--output".into(),
                "json".into(),
            ];
            if let Some(kp) = &self.config.key_pair {
                args.push("--key-name".into());
                args.push(kp.clone());
            }
            if self.config.spot {
                // P0-43 — full SpotOptions envelope. The prior
                // `MarketType=spot` form took every AWS default,
                // which means `SpotInstanceType=one-time` (the
                // instance can never be recovered) and
                // `InstanceInterruptionBehavior=terminate` (the
                // local SSM/CloudWatch state vanishes the moment
                // capacity reclaims). For the harness's
                // single-instance-per-task model we want
                // `persistent` so AWS will re-launch the same
                // request once capacity returns, and `stop` so the
                // EBS root volume + cached container images
                // survive a reclaim.
                //
                // Proactive capacity-rebalance handling: the EC2
                // `--launch-template` form gates `CapacityRebalance`
                // behind an Auto Scaling Group / Fleet (not
                // available on a single-instance `run-instances`),
                // so the equivalent semantics live in
                // `do_ensure_alive`: it polls
                // `describe-instances` each iteration and treats
                // any `instance-rebalance-recommendation` event as
                // a trigger to `release` + reprovision before AWS
                // reclaims the host. P1-46 documents the trade-off
                // and consolidates the spot-options builder.
                args.push("--instance-market-options".into());
                args.push(super::super::spot_policy::spot_market_options_arg().to_string());
            }
            // Force
            // IMDSv2 on every newly-launched instance. `HttpTokens=required`
            // makes the metadata service refuse unauthenticated requests
            // (closes SSRF + on-host metadata exfiltration vectors);
            // `HttpPutResponseHopLimit=2` matches the cluster-networking
            // recommendation so multi-container hosts can still rotate
            // the token without blocking on the default `1`; and
            // `HttpEndpoint=enabled` makes the IMDS state explicit
            // (some accounts default `disabled`, which would break
            // the agent's `aws sts get-caller-identity` startup probe).
            args.push("--metadata-options".into());
            args.push("HttpTokens=required,HttpPutResponseHopLimit=2,HttpEndpoint=enabled".into());
            // `--client-token` for AWS-side idempotency. A transient
            // network error mid-API-call can't double-launch within
            // this process; the second call returns the original
            // instance instead. Cross-process / cross-restart dedup
            // is left to the orphan-reap path.
            let token = compose_client_token(&self.run_id, &subnet_id);
            args.push("--client-token".into());
            args.push(token);
            let arg_refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
            match self.run_aws(&arg_refs) {
                Ok(stdout) => {
                    let v: serde_json::Value = serde_json::from_str(&stdout)
                        .with_context(|| format!("parsing run-instances output: {}", stdout))?;
                    let instance_id = v["Instances"][0]["InstanceId"]
                        .as_str()
                        .ok_or_else(|| anyhow!("run-instances missing InstanceId"))?;
                    launched = Some((instance_id.to_string(), subnet_id));
                    break;
                }
                Err(e) => {
                    let msg = format!("{:#}", e);
                    if msg.contains("InsufficientInstanceCapacity") {
                        last_error = Some(e);
                        continue;
                    }
                    return Err(e);
                }
            }
        }

        let (instance_id, _subnet_id) = launched.ok_or_else(|| {
            anyhow!(
                "all subnets exhausted; last AWS error: {}",
                last_error
                    .map(|e| format!("{:#}", e))
                    .unwrap_or_else(|| "(none)".into())
            )
        })?;

        self.instance = Some(ProvisionedInstance {
            instance_id,
            instance_type,
        });
        // P0-39 — refresh the stall-monitor mirror so the polling
        // thread observes the new instance on its next iteration.
        self.sync_live_instance_mirror();
        Ok(())
    }

    pub(super) fn do_ensure_alive(&mut self, dag: &DAG) -> Result<()> {
        // Called by the harness main loop before each `run_iteration`.
        // Semantics:
        // * No instance recorded yet → provision + wait_for_ssm.
        // * Instance state is `terminated` / `stopped` / `stopping` /
        // `shutting-down` → release any stale handle, then
        // reprovision (covers spot interruption + manual terminate).
        // * An `Events` entry reports a CapacityRebalance notification
        // AND spot was requested → release + reprovision proactively
        // before the existing spot gets reclaimed.
        // * Otherwise → no-op.
        if self.instance.is_none() {
            self.do_provision(dag)?;
            let ssm_wait = Duration::from_secs(120);
            self.wait_for_ssm(ssm_wait)?;
            return Ok(());
        }
        let instance_id = self
            .instance
            .as_ref()
            .map(|i| i.instance_id.clone())
            .unwrap_or_default();
        if instance_id.is_empty() {
            return Ok(());
        }

        let stdout = self.run_aws(&[
            "ec2",
            "describe-instances",
            "--instance-ids",
            &instance_id,
            "--output",
            "json",
        ])?;
        let v: serde_json::Value = serde_json::from_str(&stdout).with_context(|| {
            format!("parsing describe-instances for {}: {}", instance_id, stdout)
        })?;

        let state = v["Reservations"][0]["Instances"][0]["State"]["Name"]
            .as_str()
            .unwrap_or("")
            .to_string();
        let events = v["Reservations"][0]["Instances"][0]["Events"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let capacity_rebalance = events.iter().any(|e| {
            e["Code"]
                .as_str()
                .unwrap_or("")
                .eq_ignore_ascii_case("instance-rebalance-recommendation")
                || e["Code"]
                    .as_str()
                    .unwrap_or("")
                    .contains("CapacityRebalance")
        });

        let dead = matches!(
            state.as_str(),
            "terminated" | "stopped" | "stopping" | "shutting-down"
        );
        let spot_interrupt_proactive = self.config.spot && capacity_rebalance;

        if dead || spot_interrupt_proactive {
            eprintln!(
                "[aws] ensure_alive: instance {} state={} rebalance={} — reprovisioning",
                instance_id, state, capacity_rebalance
            );
            // Release before clearing so the terminate-instances call
            // fires against the correct id; then wipe so provision()
            // can replace it.
            self.do_release();
            self.instance = None;
            // P0-39 — defensive sync; do_release already cleared the
            // mirror but the explicit reset above is the contract.
            self.sync_live_instance_mirror();
            self.do_provision(dag)?;
            let ssm_wait = Duration::from_secs(120);
            self.wait_for_ssm(ssm_wait)?;
        }
        Ok(())
    }

    pub(super) fn do_release(&mut self) {
        let Some(inst) = self.instance.take() else {
            return;
        };
        // Clear the stall-monitor mirror before the network
        // round-trip so the polling thread observes `None` on its very
        // next iteration even if `terminate-instances` blocks.
        self.sync_live_instance_mirror();
        // Clear the advertised network capability so the next
        // `do_provision` re-evaluates `required_network_floor` against
        // a fresh DAG view.
        if let Ok(mut g) = self.effective_network.lock() {
            *g = None;
        }
        let _ = self.run_aws(&[
            "ec2",
            "terminate-instances",
            "--instance-ids",
            &inst.instance_id,
            "--output",
            "json",
        ]);
    }

    /// AWS pilot path. When enabled, the pilot (in order):
    ///
    /// 1. Saves the currently-provisioned instance (if any) so the
    ///    caller's prior state can be restored on failure.
    /// 2. Sets `pilot_instance_override = Some(cfg.pilot_instance_type)`
    ///    so the subsequent `provision` call uses the pilot shape
    ///    (default `t3.medium`) regardless of DAG sizing profiles.
    /// 3. Calls `self.provision(dag)` to launch the pilot instance.
    ///    Any provision error falls through to `Ok(None)` — pilot
    ///    becomes a no-op and the main loop proceeds with baseline
    ///    sizing, per the "pilot never blocks execution" contract.
    /// 4. Waits for SSM readiness via `wait_for_ssm` so CloudWatch
    ///    metrics are plausibly available before measurement.
    /// 5. Loads profiles + facts and selects representative pilot
    ///    tasks via `select_pilot_tasks`.
    /// 6. For each selected task shells out to `aws cloudwatch
    /// get-metric-statistics` for `CPUUtilization` +
    ///    `MemoryUtilization` against the pilot instance, collecting
    ///    `PilotMeasurement` rows.
    /// 7. Builds a `PilotReport` via `project_requirements` +
    ///    `compute_confidence`.
    /// 8. Runs `cost_guard::check_ceiling` on the projected full-run
    ///    cost. Breaches surface as the caller's error path after the
    ///    pilot instance has been released.
    /// 9. Writes pilot artefacts under `runtime/pilot/`.
    /// 10. `release()`s the pilot instance and clears the instance-type
    ///     override. After this, `self.instance` is `None` and
    ///     `pilot_instance_override` is `None`, so the main harness
    ///     loop's next `ensure_alive` / `provision` call launches a
    ///     fresh real-shape instance.
    ///
    /// **Not yet in scope:** actually executing the selected pilot
    /// tasks (SSM `RunCommand` against the pilot instance). Running
    /// the agent via SSM during pilot requires the full S3 package
    /// upload + wrapper-script plumbing already exercised by
    /// `run_iteration`; factoring that into a pilot-specific driver is
    /// tracked as a follow-up. This impl measures CloudWatch with an
    /// empty task run, so measurements are driven by idle-instance
    /// metrics — enough to exercise the wire events and the cost guard
    /// but not yet the per-stage resource projection. See the
    /// `measurements[..].exit_status == -1` sentinel path below.
    pub(super) fn do_pilot(&mut self, dag: &DAG, cfg: &PilotConfig) -> Result<Option<PilotReport>> {
        // Build the internal ProgressClient if the session endpoint was wired.
        self.ensure_progress_client();
        self.pilot_projected_requirements = None;
        if !cfg.enabled {
            return Ok(None);
        }
        let package = std::path::Path::new(&self.args.package).to_path_buf();
        // Save caller's prior state so we can restore if provision
        // fails. Normally `prior_instance` is `None` (pilot runs before
        // the main loop's provision), but tests and re-entry paths can
        // set it, so we handle both cases.
        let prior_instance = self.instance.take();
        // P0-39 — keep the monitor mirror aligned with the take.
        self.sync_live_instance_mirror();

        // Install the pilot override so the subsequent `provision`
        // call sees `cfg.pilot_instance_type` instead of the DAG's
        // real sizing. Cleared before this function returns via every
        // exit path.
        self.pilot_instance_override = Some(cfg.pilot_instance_type.clone());

        // Provision the dedicated pilot instance. On any failure,
        // restore prior state and fall through to `Ok(None)` so the
        // pilot is observationally a no-op — the main loop proceeds
        // with baseline sizing.
        if let Err(e) = self.do_provision(dag) {
            eprintln!("[aws] pilot: provision failed, skipping pilot: {:#}", e);
            self.pilot_instance_override = None;
            self.instance = prior_instance;
            self.sync_live_instance_mirror();
            return Ok(None);
        }

        // Wait for SSM. A failure here is recoverable: release the
        // pilot instance, restore state, and skip the pilot. This
        // avoids leaking an instance when CloudWatch wouldn't have
        // given us useful data anyway.
        if let Err(e) = self.wait_for_ssm(Duration::from_secs(120)) {
            eprintln!("[aws] pilot: wait_for_ssm failed: {:#}", e);
            self.do_release();
            self.pilot_instance_override = None;
            self.instance = prior_instance;
            self.sync_live_instance_mirror();
            return Ok(None);
        }

        // From here on we've got a live pilot instance and must
        // guarantee `release()` runs before we return. Use an inner
        // closure so error paths can still go through cleanup.
        let result = self.pilot_measure_and_report(dag, cfg, &package);

        // Always release the pilot instance + clear the override so
        // the main loop's next provision runs fresh. `release` sets
        // `self.instance = None` via `take`, which is exactly what the
        // contract promises.
        self.do_release();
        self.pilot_instance_override = None;
        // Intentionally do NOT restore `prior_instance`: the pilot
        // contract is that the main loop re-provisions the real
        // instance via `ensure_alive` after pilot returns. Keeping
        // `self.instance = None` makes that unambiguous.

        result
    }

    /// Inner body of `pilot` that runs after the pilot instance is up
    /// and SSM-reachable. Extracted so the outer
    /// `pilot` function can guarantee `release()` + override-clearing
    /// regardless of this closure's success or error result.
    fn pilot_measure_and_report(
        &mut self,
        dag: &DAG,
        cfg: &PilotConfig,
        package: &Path,
    ) -> Result<Option<PilotReport>> {
        let profiles = load_aws_profiles(package)?;
        let facts = load_aws_facts(package);

        // Select the representative pilot task ids. When the Ready set
        // is empty (e.g. fresh emit with no ready tasks) the pilot
        // still emits a report so the wire events fire.
        let picks = super::super::pilot::select_pilot_tasks(dag, &profiles, &facts, cfg);

        eprintln!(
            "[aws] pilot: provisioned {} ({} task(s) selected: {:?})",
            cfg.pilot_instance_type,
            picks.len(),
            picks
        );

        // Measurements are driven by CloudWatch readings against the
        // pilot instance. Actually running the pilot tasks (SSM
        // RunCommand) is a follow-up — see the pilot() doc comment.
        let instance_id = self.instance.as_ref().map(|i| i.instance_id.clone());
        let mut measurements: Vec<PilotMeasurement> = Vec::new();
        for task_id in &picks {
            let Some(task) = dag.tasks.get(task_id.as_str()) else {
                continue;
            };
            let stage_class = task_stage_class(task);
            let peak_rss_mb = instance_id
                .as_deref()
                .and_then(|iid| self.cloudwatch_max("MemoryUtilization", iid).ok())
                .unwrap_or(0);
            let peak_cpu_pct = instance_id
                .as_deref()
                .and_then(|iid| self.cloudwatch_max("CPUUtilization", iid).ok())
                .unwrap_or(0);
            // `exit_status = -1` signals "no task actually executed" so
            // downstream consumers can distinguish an observed failure
            // from a projection run. Replaced with a real exit code
            // when the follow-up pilot SSM RunCommand lands.
            let exit_status = if peak_rss_mb == 0 && peak_cpu_pct == 0 {
                -1
            } else {
                0
            };
            measurements.push(PilotMeasurement {
                task_id: task_id.clone(),
                stage_class,
                peak_rss_mb,
                wall_time_secs: 0,
                disk_used_mb: 0,
                exit_status,
            });
        }

        let projected =
            super::super::pilot::project_requirements(dag, &profiles, &facts, &measurements, cfg);
        // Confidence: zero when every measurement carries the sentinel
        // -1 (CloudWatch empty / agent missing). Otherwise delegate to
        // the shared computer.
        let confidence =
            if !measurements.is_empty() && measurements.iter().all(|m| m.exit_status == -1) {
                0.0
            } else {
                super::super::pilot::compute_confidence(&measurements, &profiles, &facts)
            };
        let report = PilotReport {
            measurements,
            projected_requirements: projected.clone(),
            confidence,
        };

        // Estimate the full-run cost using the projected shapes and
        // gate via the cost ceiling. Breach bubbles to the caller —
        // the outer `pilot` still runs `release()` + override-clearing
        // on the way out, so the pilot instance does not leak.
        let pricing_source = if self.config.spot {
            PricingSource::Spot
        } else {
            PricingSource::OnDemand
        };
        let planned_hours_per_stage = (self.args.task_timeout_secs as f64) / 3600.0;
        let cost_pairs: Vec<(String, f64)> = projected
            .values()
            .map(|req| {
                let it = super::super::sizing::resolve_instance_type(req);
                (it, planned_hours_per_stage)
            })
            .collect();
        if !cost_pairs.is_empty() {
            let estimate = super::super::cost_guard::estimate_for_backend(
                self.cost_backend,
                &cost_pairs,
                pricing_source,
            )
            .map_err(|e| anyhow!("pilot cost estimate failed: {}", e))?;
            super::super::cost_guard::check_ceiling_for_backend(self.cost_backend, estimate)
                .map_err(|e| anyhow!("pilot cost ceiling breached: {}", e))?;

            // Emit cost_guard_passed so operators see the pilot-projected
            // full-run estimate passed the ceiling check. No cumulative
            // tracking here (the pilot is pre-provision); use the
            // persisted cumulative as context.
            if let (Some(ref pc), Some(est)) = (&self.progress_client, estimate) {
                let ceiling_usd =
                    super::super::cost_guard::read_provision_ceiling_usd().unwrap_or(0.0);
                // W5.1: prefer the strict tracker; if it errors because
                // the env var is unset, fall back to the legacy default
                // here only — this branch is diagnostic event emission,
                // not provisioning, so the silent default is acceptable
                // context display.
                let cumulative = match super::super::cost_guard::CumulativeSpend::for_package_strict(
                    &self.args.package,
                ) {
                    Ok(cs) => cs,
                    Err(_) => {
                        super::super::cost_guard::CumulativeSpend::for_package(&self.args.package)
                    }
                };
                let current = cumulative.current_cumulative().unwrap_or(0.0);
                super::super::cost_guard::emit_cost_guard_passed(
                    pc,
                    "",
                    est,
                    ceiling_usd,
                    current,
                    cumulative.ceiling_usd(),
                );
            }
        }

        super::super::pilot::write_pilot_artifacts(package, &report)?;
        self.pilot_projected_requirements = Some(projected);
        Ok(Some(report))
    }
}

#[cfg(test)]
mod compose_client_token_tests {
    use super::compose_client_token;

    #[test]
    fn short_token_passes_through_unchanged() {
        let run_id = "abcdef0123456789";
        let subnet = "subnet-aaaa1111";
        let token = compose_client_token(run_id, subnet);
        assert_eq!(token, format!("{}-{}", run_id, subnet));
        assert!(token.len() <= 64, "short composition must be untouched");
    }

    #[test]
    fn token_under_64_char_aws_limit() {
        // 32-char UUID-simple run id + dash + 64-char subnet would
        // exceed; ensure the helper truncates.
        let run_id = "0123456789abcdef0123456789abcdef";
        let subnet = "subnet-very-long-name-deliberately-padded-to-test-truncation";
        let token = compose_client_token(run_id, subnet);
        assert!(
            token.len() <= 64,
            "token must respect the 64-char AWS limit, got {} chars",
            token.len()
        );
    }

    #[test]
    fn same_inputs_yield_same_token_idempotent() {
        // Idempotency contract: a retry of the same logical request
        // (same run_id, same subnet) must produce the same token so
        // AWS dedups.
        let run_id = "fixedrunid";
        let subnet = "subnet-fixed";
        assert_eq!(
            compose_client_token(run_id, subnet),
            compose_client_token(run_id, subnet),
            "deterministic on identical inputs"
        );
    }

    #[test]
    fn different_subnets_yield_different_tokens() {
        let run_id = "fixedrunid";
        let a = compose_client_token(run_id, "subnet-a");
        let b = compose_client_token(run_id, "subnet-b");
        assert_ne!(a, b, "different subnets must yield different tokens");
    }

    #[test]
    fn long_inputs_still_unique_after_truncation() {
        // After the FNV hash fallback, two different long composites
        // should still produce distinct tokens.
        let run_id = "x".repeat(40);
        let token_a = compose_client_token(&run_id, "subnet-aaaaaaaaaaaa-pad");
        let token_b = compose_client_token(&run_id, "subnet-bbbbbbbbbbbb-pad");
        assert_ne!(
            token_a, token_b,
            "truncation must preserve uniqueness via hash suffix"
        );
    }
}

#[cfg(test)]
mod allowlist_tests {
    // The `set/remove_var` calls inside these tests are flagged
    // as unsafe in Rust 2024 edition because the
    // env table is not thread-safe; all callsites are single-threaded
    // setup gated by the crate-wide SWFC_AWS_ENV_LOCK.
    #![allow(unsafe_code)]
    use super::super::super::SWFC_AWS_ENV_LOCK;
    use super::check_instance_type_allowed;

    fn with_allowlist<T>(value: Option<&str>, body: impl FnOnce() -> T) -> T {
        let _lock = SWFC_AWS_ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let prior = std::env::var("SWFC_AWS_INSTANCE_TYPE_ALLOWLIST").ok();
        match value {
            Some(v) => unsafe { std::env::set_var("SWFC_AWS_INSTANCE_TYPE_ALLOWLIST", v) },
            None => unsafe { std::env::remove_var("SWFC_AWS_INSTANCE_TYPE_ALLOWLIST") },
        }
        let out = body();
        match prior {
            Some(v) => unsafe { std::env::set_var("SWFC_AWS_INSTANCE_TYPE_ALLOWLIST", v) },
            None => unsafe { std::env::remove_var("SWFC_AWS_INSTANCE_TYPE_ALLOWLIST") },
        }
        out
    }

    #[test]
    fn unset_allowlist_permits_any_type() {
        with_allowlist(None, || {
            assert!(check_instance_type_allowed("t3.medium").is_ok());
            assert!(check_instance_type_allowed("p4d.24xlarge").is_ok());
        });
    }

    #[test]
    fn empty_allowlist_treated_as_unset() {
        // `SWFC_AWS_INSTANCE_TYPE_ALLOWLIST=""` is what `unset -v` in
        // a Makefile or `.env` reset looks like; treat as None.
        with_allowlist(Some(""), || {
            assert!(check_instance_type_allowed("t3.medium").is_ok());
        });
    }

    #[test]
    fn allowlist_admits_matching_type() {
        with_allowlist(Some("t3.medium,m6i.large,c6i.xlarge"), || {
            assert!(check_instance_type_allowed("m6i.large").is_ok());
        });
    }

    #[test]
    fn allowlist_rejects_unlisted_type() {
        with_allowlist(Some("t3.medium,m6i.large"), || {
            let err = check_instance_type_allowed("p4d.24xlarge").unwrap_err();
            let msg = format!("{:#}", err);
            assert!(
                msg.contains("p4d.24xlarge"),
                "diagnostic must echo bad type: {msg}"
            );
            assert!(
                msg.contains("SWFC_AWS_INSTANCE_TYPE_ALLOWLIST"),
                "diagnostic must name the env var: {msg}"
            );
        });
    }

    #[test]
    fn allowlist_tolerates_whitespace_in_entries() {
        with_allowlist(Some(" t3.medium , m6i.large "), || {
            assert!(check_instance_type_allowed("t3.medium").is_ok());
            assert!(check_instance_type_allowed("m6i.large").is_ok());
        });
    }
}

#[cfg(test)]
mod run_id_persistence_tests {
    //! `load_or_create_run_id` round-trip.
    use super::load_or_create_run_id;

    #[test]
    fn creates_sidecar_on_first_call() {
        let tmp = tempfile::tempdir().unwrap();
        let pkg = tmp.path();
        let id = load_or_create_run_id(pkg);
        assert_eq!(id.len(), 32, "run_id must be 32 hex chars: {id:?}");
        assert!(id.chars().all(|c| c.is_ascii_hexdigit()));
        assert!(pkg.join("runtime/.harness-run-id").exists());
    }

    #[test]
    fn returns_same_id_on_second_call() {
        let tmp = tempfile::tempdir().unwrap();
        let pkg = tmp.path();
        let id1 = load_or_create_run_id(pkg);
        let id2 = load_or_create_run_id(pkg);
        assert_eq!(id1, id2, "crash-restart must reuse the persisted run_id");
    }

    #[test]
    fn regenerates_on_corrupted_sidecar() {
        let tmp = tempfile::tempdir().unwrap();
        let pkg = tmp.path();
        std::fs::create_dir_all(pkg.join("runtime")).unwrap();
        std::fs::write(pkg.join("runtime/.harness-run-id"), "not-a-real-uuid").unwrap();
        let id = load_or_create_run_id(pkg);
        assert_eq!(id.len(), 32);
        // Sidecar replaced with the fresh id.
        let on_disk = std::fs::read_to_string(pkg.join("runtime/.harness-run-id")).unwrap();
        assert_eq!(on_disk.trim(), id);
    }

    #[test]
    fn tolerates_unwritable_pkg() {
        // Pointing at a path under a nonexistent root means
        // create_dir_all + write both fail; the function must still
        // return a fresh id rather than panic.
        let id = load_or_create_run_id(std::path::Path::new("/nonexistent/path/12345"));
        assert_eq!(id.len(), 32);
    }
}
