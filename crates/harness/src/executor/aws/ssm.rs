//! SSM-driven per-task execution + staleness checking.
//!
//! Owns the `run_iteration` body (send-command + list-command-invocations
//! poll), `is_task_stale` + its SSM round-trip half `query_ssm_stale`,
//! and the per-stage timeout resolver.

use super::super::sizing::{resolve_ssm_timeout_secs, ComputeProfiles};
use super::super::{IterationOutcome, RemoteExecutionInfo};
use super::sizing::{read_dag, task_id_from_spec, task_stage_class, write_dag};
use super::AwsExecutor;
use crate::constants::OUTPUT_TAIL_BYTES;
use anyhow::{anyhow, Context, Result};
use ecaa_workflow_core::dag::{Task, TaskState};
use ecaa_workflow_core::ids::TaskId;
use std::os::unix::process::ExitStatusExt;
use std::path::Path;
use std::process::ExitStatus;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

/// Per-task SSM staleness cache TTL. At the harness's 5 s iteration
/// cadence, without caching every running task issues an SSM
/// `list-command-invocations` round-trip each iteration; at 50 running
/// tasks with ~30 s SSM latency the check alone serializes into ~25 s
/// of wall time per iteration. Caching for 30 s lets the common
/// steady-state answer amortize across ~6 iterations.
pub(super) const SSM_STALE_CACHE_TTL_SECS: u64 = 30;

/// P1-44 — env vars that must NEVER appear in an SSM `send-command
/// --parameters` payload. AWS persists the parameters JSON in
/// CloudTrail for 30 days and exposes it via `ssm:GetCommandInvocation`
/// to anyone holding the IAM action. A literal API token in the
/// command body is therefore a 30-day disclosure to every IAM
/// principal with read access on the management account — far beyond
/// the per-task surface the agent should ever see.
///
/// The harness refuses to build a payload containing any of these
/// keys. The remote agent must obtain them via a different path:
///
///   * `ECAA_ANTHROPIC_API_KEY` / `ANTHROPIC_API_KEY` — staged onto
///     the instance under `/etc/ecaa-workflow/credentials/`
///     (mode 0600, owner ssm-user) by the AMI bootstrap and read by
///     `run-task-on-instance.sh` before invoking the agent.
///   * `AWS_*` — the EC2 instance profile already supplies these via
///     IMDSv2 metadata; the harness never needs to ship credentials
///     into the SSM envelope.
///   * `GITHUB_TOKEN` / `GH_TOKEN` — sourced by the agent from the
///     same instance-profile-attached Secrets Manager bundle.
///
/// Spec follow-up (out of scope here): an S3-presigned-URL +
/// instance-profile-read mechanic would let the harness stage
/// ephemeral session credentials per-task without baking them into
/// the AMI. Until that lands, refusing the secret-bearing envelope
/// keeps the disclosure surface bounded.
///
/// W3.1: the curated list is the union of
/// `super::super::_secrets::BASE_SECRET_KEYS` (shared with local
/// executor) plus `AWS_EXTRA_SECRET_KEYS` (AWS-only:
/// GITHUB_PERSONAL_ACCESS_TOKEN, HF_TOKEN). Drift between local + AWS
/// is no longer possible because adding a credential to the base list
/// automatically scrubs it from both env paths.
fn is_curated_secret(name: &str) -> Option<&'static str> {
    super::super::_secrets::aws_secret_keys()
        .find(|s| **s == name)
        .copied()
}

/// SSM `send-command` parameters are persisted by AWS for ~30 days and
/// readable by any IAM principal holding `ssm:GetCommandInvocation`. This
/// function strips env vars from the envelope using two complementary modes:
///
/// 1. **Curated list** (`SECRET_KEYS`): exact-name matches for well-known
///    credential vars that may not follow a predictable naming convention
///    (e.g. `GH_TOKEN`, `HF_TOKEN`, `ECAA_ANTHROPIC_API_KEY`). The curated
///    list is the primary false-negative defence — adding a new well-known
///    credential here is always correct regardless of its suffix.
///
/// 2. **Pattern match**: catches naming-convention variants the curated list
///    can't enumerate (e.g. `GITHUB_TOKEN_BACKUP`, `MY_STRIPE_SECRET`,
///    `INTERNAL_API_KEY`). A var is pattern-matched secret if its uppercased
///    name *contains* any of: `_TOKEN`, `_KEY`, `_SECRET`, `_PASSWORD`,
///    `_CREDENTIAL`, `_API_KEY`, `_ACCESS_KEY`, `_SESSION_TOKEN`. The check
///    is substring-based so prefix and suffix variants both match.
///
/// Both modes must agree to pass: a var is dropped if EITHER the curated list
/// OR the pattern check flags it. Returns a sanitized envelope plus the list
/// of curated-list keys that were dropped (pattern-dropped keys use a sentinel
/// string so the caller can warn without logging values).
pub(super) fn filter_secrets(
    envelope: &std::collections::BTreeMap<String, String>,
) -> (
    std::collections::BTreeMap<String, String>,
    Vec<&'static str>,
) {
    /// Substrings (checked against the uppercased key name) that identify
    /// a variable as credential-shaped. A match here supplements the curated
    /// `SECRET_KEYS` list so that naming-convention variants (e.g.
    /// `GITHUB_TOKEN_BACKUP`, `MY_STRIPE_SECRET`) are caught even when the
    /// exact name was never added to the curated list.
    const SECRET_PATTERNS: &[&str] = &[
        "_TOKEN",
        "_KEY",
        "_SECRET",
        "_PASSWORD",
        "_CREDENTIAL",
        "_API_KEY",
        "_ACCESS_KEY",
        "_SESSION_TOKEN",
    ];

    let mut safe = std::collections::BTreeMap::new();
    let mut dropped: Vec<&'static str> = Vec::new();
    for (k, v) in envelope {
        // Mode 1: curated exact-name list (W3.1 — sourced from
        // `super::super::_secrets::aws_secret_keys()` so the AWS path
        // can't drift from the local executor's allowlist).
        if let Some(secret_key) = is_curated_secret(k) {
            dropped.push(secret_key);
            continue;
        }
        // Mode 2: pattern match on uppercased name.
        let upper = k.to_uppercase();
        if SECRET_PATTERNS.iter().any(|pat| upper.contains(pat)) {
            // Use a static sentinel string so the caller can warn without
            // needing the original key (which might itself be sensitive).
            dropped.push("<pattern-matched secret>");
            continue;
        }
        safe.insert(k.clone(), v.clone());
    }
    (safe, dropped)
}

impl AwsExecutor {
    pub(super) fn do_run_iteration(
        &mut self,
        package: &Path,
        agent_cmd: &str,
        envelope: &std::collections::BTreeMap<String, String>,
    ) -> Result<IterationOutcome> {
        // SSM RunCommand-driven execution. The flow:
        // 1. Read WORKFLOW.json and pick the first ready task.
        // 2. Issue `aws ssm send-command` pointing at the remote
        // wrapper (scripts/run-task-on-instance.sh) with the
        // task id + package URI + agent cmd embedded as env
        // exports in the `commands` parameter. Simplest thing
        // that works — we avoid a separate SSM document-registration
        // step by piggy-backing on AWS-RunShellScript.
        // 3. Poll `aws ssm list-command-invocations` until status
        // is terminal (Success / Failed / Cancelled / TimedOut).
        // 4. Exit code 0 => TaskCompleted, non-zero => TaskFailed.
        // NoReadyTasks / AllDone are signalled by returning a
        // success outcome with no remote info — the harness
        // main loop already handles completion detection via
        // after.is_complete() + the no-progress streak.
        let instance = self
            .instance
            .as_ref()
            .ok_or_else(|| anyhow!("run_iteration called before provision"))?
            .clone();
        let dag = read_dag(package)?;
        let ready: Vec<(String, Task)> = dag
            .tasks
            .iter()
            .filter(|(_, t)| matches!(t.state, TaskState::Ready))
            .map(|(id, t)| (id.to_string(), t.clone()))
            .collect();

        let Some((task_id, task)) = ready.into_iter().next() else {
            // No ready tasks: either the DAG is complete or every
            // remaining task is pending/blocked. Fall through to the
            // harness main loop, which already handles both cases via
            // is_complete() and the no-progress streak counter.
            *self.current_running_task_id.lock().unwrap() = None;
            return Ok(IterationOutcome {
                agent_status: ExitStatus::from_raw(0),
                remote: None,
            });
        };

        // Publish the current task id so the stall monitor's emitted
        // signals carry it. Reset when the iteration exits below.
        *self.current_running_task_id.lock().unwrap() = Some(task_id.clone());

        // Resolve per-task SSM timeout from the emitted compute profiles.
        let stage_class = task_stage_class(&task);
        let timeout_secs = resolve_ssm_timeout_for_stage(&self.args.package, &stage_class);

        // Build the SSM parameters. The wrapper reads ECAA_TASK_ID /
        // ECAA_S3_PACKAGE_URI / ECAA_AGENT_CMD / ECAA_S3_OUTPUT_URI
        // from its env, so we export them inline before invoking it.
        // S3 URIs come from the existing ECAA_AWS_S3_BUCKET /
        // ECAA_AWS_S3_PREFIX env vars; if unset we fall back to
        // placeholder values that make the missing configuration
        // obvious in the recorded SSM log.
        let s3_bucket =
            std::env::var("ECAA_AWS_S3_BUCKET").unwrap_or_else(|_| "ecaa-workflow".into());
        let s3_prefix =
            std::env::var("ECAA_AWS_S3_PREFIX").unwrap_or_else(|_| "ecaa-workflow/".into());
        let package_name = Path::new(package)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("package");
        let package_uri = format!(
            "s3://{}/{}{}",
            s3_bucket.trim_end_matches('/'),
            s3_prefix,
            package_name
        );
        let output_uri = format!(
            "{}/runtime/outputs/{}",
            package_uri.trim_end_matches('/'),
            task_id
        );

        // One commands[] entry that exports env vars and runs the wrapper.
        // Shell quoting is deliberately minimal — these values come from
        // trusted config, not untrusted SME input. Phase-2 hardware
        // envelope vars are prepended as additional `KEY=VALUE` exports
        // so the agent on the remote instance reads them the same way
        // the local agent does.
        //
        // Per-task remediation overrides (library_pins, env_passthrough)
        // accumulated by `apply_overrides` ride into the envelope here so
        // the remote agent gets `ECAA_LIB_PIN_*` etc. The drain is
        // unconditional — if no overrides are pending the map is empty.
        let remote_script = "/opt/ecaa-workflow/run-task-on-instance.sh";
        let mut effective_envelope = envelope.clone();
        for (k, v) in std::mem::take(&mut self.pending_envelope_additions) {
            effective_envelope.entry(k).or_insert(v);
        }
        // P1-44 — refuse to land secret-bearing env vars in the SSM
        // `--parameters` payload (durable in CloudTrail for 30 days,
        // readable by every `ssm:GetCommandInvocation` IAM principal).
        // The agent gets credentials via the EC2 instance profile +
        // AMI-staged credentials file, never via the SSM envelope.
        let (effective_envelope, dropped_secrets) = filter_secrets(&effective_envelope);
        for k in &dropped_secrets {
            tracing::warn!(
                target: "ecaa::aws::ssm",
                env_key = %k,
                task_id = %task_id,
                "refusing to ship credential env var via SSM envelope; \
                 agent must read it from /etc/ecaa-workflow/credentials/"
            );
        }
        let mut command_line = String::new();
        for (k, v) in effective_envelope.iter() {
            // JSON values (tool_thread_curves, env_overrides, intake_facts)
            // contain `"` and `{}` so we single-quote each value and escape
            // any embedded single-quotes via the shell `'\''` idiom.
            command_line.push_str(k);
            command_line.push('=');
            command_line.push('\'');
            command_line.push_str(&v.replace('\'', "'\\''"));
            command_line.push_str("' ");
        }
        command_line.push_str(&format!(
            "ECAA_TASK_ID={} ECAA_S3_PACKAGE_URI={} ECAA_AGENT_CMD={} ECAA_S3_OUTPUT_URI={} {}",
            task_id, package_uri, agent_cmd, output_uri, remote_script
        ));
        // The commands[] slot is a JSON string so its own embedded
        // double quotes need escaping. serde_json::to_string handles
        // that + the single-quoted values.
        let parameters = serde_json::to_string(&serde_json::json!({
            "commands": [command_line]
        }))
        .context("serializing SSM parameters")?;

        // Mark the task as Running with remote metadata *before* dispatch
        // so a crash between send-command and the poll loop still leaves
        // WORKFLOW.json in a recoverable state — is_task_stale will
        // consult SSM the next iteration and decide what to do.
        {
            let mut dag_mut = dag;
            if let Some(t) = dag_mut.tasks.get_mut(task_id.as_str()) {
                t.state = TaskState::Running {
                    started_at: chrono::Utc::now().to_rfc3339(),
                    remote: Some(ecaa_workflow_core::dag::RemoteExecution {
                        backend: "aws".to_string(),
                        instance_id: instance.instance_id.clone(),
                        instance_type: instance.instance_type.clone(),
                        command_id: None,
                        output_uri: Some(output_uri.clone()),
                    }),
                };
            }
            write_dag(package, &dag_mut)?;
        }

        // 2. Dispatch the SSM RunCommand.
        let send_stdout = self.run_aws(&[
            "ssm",
            "send-command",
            "--document-name",
            "AWS-RunShellScript",
            "--instance-ids",
            &instance.instance_id,
            "--parameters",
            &parameters,
            "--timeout-seconds",
            &timeout_secs.to_string(),
            "--comment",
            &format!("scripps-task-{}", task_id),
            "--output",
            "json",
        ])?;
        let send_json: serde_json::Value = serde_json::from_str(&send_stdout)
            .with_context(|| format!("parsing send-command output: {}", send_stdout))?;
        let command_id = send_json["Command"]["CommandId"]
            .as_str()
            .ok_or_else(|| anyhow!("send-command missing Command.CommandId: {}", send_stdout))?
            .to_string();

        // 3. Poll list-command-invocations. We prefer list-command-invocations
        // over get-command-invocation because the latter returns NotFound
        // for a short window right after dispatch on some regions; list
        // handles the race gracefully by returning an empty array.
        let poll_interval = Duration::from_secs(5);
        let deadline = Instant::now() + Duration::from_secs(timeout_secs);
        let status_raw = loop {
            let stdout = self.run_aws(&[
                "ssm",
                "list-command-invocations",
                "--command-id",
                &command_id,
                "--instance-id",
                &instance.instance_id,
                "--details",
                "--output",
                "json",
            ])?;
            let v: serde_json::Value = serde_json::from_str(&stdout)
                .with_context(|| format!("parsing list-command-invocations: {}", stdout))?;
            let invocation = v["CommandInvocations"].get(0).cloned();
            if let Some(inv) = invocation {
                let status = inv["Status"].as_str().unwrap_or("").to_string();
                match status.as_str() {
                    "Success" | "Failed" | "Cancelled" | "TimedOut" => {
                        break (status, inv);
                    }
                    _ => {}
                }
            }
            if Instant::now() >= deadline {
                // Hard timeout: mark Failed and return — the harness's
                // stale-detection path will also catch this next loop
                // but we fail fast here to free the iteration slot.
                break (
                    "TimedOut".to_string(),
                    serde_json::json!({
                        "Status": "TimedOut",
                        "StandardErrorContent": format!(
                            "ecaa-workflow: SSM RunCommand exceeded {}s timeout",
                            timeout_secs
                        ),
                    }),
                );
            }
            // Cooperative shutdown: SIGINT handler sets this flag via
            // `release_in_handler(&self)` without acquiring the iteration
            // mutex. Returning early here lets `run_iteration` exit so
            // the main loop drops the mutex before the full `release` runs.
            if self.shutdown_requested.load(Ordering::Acquire) {
                break (
                    "TimedOut".to_string(),
                    serde_json::json!({
                        "Status": "TimedOut",
                        "StandardErrorContent":
                            "ecaa-workflow: SSM poll interrupted by shutdown signal",
                    }),
                );
            }
            std::thread::sleep(poll_interval);
        };
        let (status, invocation) = status_raw;

        // 4. Translate status + exit code into WORKFLOW.json state.
        let response_code = invocation["ResponseCode"].as_i64().unwrap_or(-1);
        let stderr_content = invocation["StandardErrorContent"]
            .as_str()
            .unwrap_or("")
            .to_string();
        let stdout_content = invocation["StandardOutputContent"]
            .as_str()
            .unwrap_or("")
            .to_string();
        let success = status == "Success" && response_code == 0;
        let mut dag_mut = read_dag(package)?;
        if let Some(t) = dag_mut.tasks.get_mut(task_id.as_str()) {
            if success {
                t.state = TaskState::Completed {
                    result: serde_json::json!({
                        "backend": "aws",
                        "instance_id": instance.instance_id,
                        "instance_type": instance.instance_type,
                        "region": self.config.region,
                        "ssm_command_id": command_id,
                        "ssm_status": status,
                        "stdout_tail": tail_bytes(&stdout_content, OUTPUT_TAIL_BYTES),
                    }),
                };
            } else {
                t.state = TaskState::Failed {
                    reason: format!(
                        "AWS SSM RunCommand status={} response_code={} stderr={}",
                        status,
                        response_code,
                        tail_bytes(&stderr_content, OUTPUT_TAIL_BYTES)
                    ),
                };
            }
        }
        // Incremental propagation: exactly one task transitioned in this
        // run_iteration call — to Completed (success) or Failed
        // (failure). Either way, downstream propagation only needs to
        // re-evaluate dependents of `task_id`.
        dag_mut.propagate_readiness_from(&[TaskId::from(task_id.as_str())]);
        write_dag(package, &dag_mut)?;

        // 5. Return an IterationOutcome that reflects the agent's exit
        // status + attaches remote info for the harness's progress
        // event emitter.
        //
        // Stash an `IterationCapture` so the harness main can synthesise
        // a `ToolErrorEnvelope` on non-zero exit. Captured fields:
        // stdout/stderr from SSM, exit code from ResponseCode, no signal
        // (SSM RunCommand doesn't surface UNIX signals; the response
        // code reflects the wrapped script's exit).
        let mut ctx = std::collections::BTreeMap::new();
        ctx.insert("executor".into(), "aws".into());
        ctx.insert("instance_id".into(), instance.instance_id.clone());
        ctx.insert("instance_type".into(), instance.instance_type.clone());
        ctx.insert("region".into(), self.config.region.clone());
        ctx.insert("ssm_command_id".into(), command_id.clone());
        ctx.insert("ssm_status".into(), status.clone());
        if let Ok(mut slot) = self.last_capture.lock() {
            *slot = Some(super::super::IterationCapture {
                stderr: stderr_content.clone(),
                stdout: stdout_content.clone(),
                exit_code: Some(response_code as i32),
                signal: None,
                wallclock_secs: None,
                peak_memory_mb: None,
                executor_context: ctx,
            });
        }

        *self.current_running_task_id.lock().unwrap() = None;
        let exit_code = if success { 0 } else { 1 };
        Ok(IterationOutcome {
            agent_status: ExitStatus::from_raw(exit_code << 8),
            remote: Some(RemoteExecutionInfo {
                backend: "aws".to_string(),
                instance_id: instance.instance_id.clone(),
                instance_type: instance.instance_type.clone(),
            }),
        })
    }

    pub(super) fn do_is_task_stale(&self, task: &Task, now_secs: u64) -> bool {
        // SSM-aware staleness: the timestamp check is
        // still the first gate (fast path for fresh tasks), but once
        // we've exceeded the per-stage ssm_timeout_secs we consult the
        // live SSM invocation status before declaring the task stale.
        //
        // The remote metadata on TaskState::Running carries no command
        // id yet (we don't write it back during run_iteration to keep
        // the WORKFLOW.json byte-diff minimal), so we query by task
        // id + instance id using the `--filters` flag:
        // - aws ssm list-command-invocations --filters
        // Key=Comment,Values=scripps-task-<task_id>
        // --instance-id <id>
        // AwsExecutor::run_iteration records a comment of that shape
        // on the send-command call so this query can find the invocation.
        let TaskState::Running { started_at, remote } = &task.state else {
            return false;
        };
        let Ok(start) = chrono::DateTime::parse_from_rfc3339(started_at) else {
            return false;
        };
        let elapsed = now_secs.saturating_sub(start.timestamp().max(0) as u64);

        // Resolve the timeout for this task's stage class — default to
        // the session-wide `task_timeout_secs` until the profile layer
        // can tell us otherwise.
        let stage_class = task_stage_class(task);
        let timeout_secs = resolve_ssm_timeout_for_stage(&self.args.package, &stage_class);

        if elapsed <= timeout_secs {
            return false;
        }

        // consult the SSM staleness cache before issuing a
        // round-trip. Keyed by task id (derived via the same helper
        // the real query uses), with a TTL so steady-state
        // observations amortize across ~6 iterations.
        let task_id_hint = task_id_from_spec(task).unwrap_or_default();
        if !task_id_hint.is_empty() {
            if let Ok(cache) = self.ssm_stale_cache.lock() {
                if let Some((cached_at, result)) = cache.get(&task_id_hint) {
                    if now_secs.saturating_sub(*cached_at) < SSM_STALE_CACHE_TTL_SECS {
                        return *result;
                    }
                }
            }
        }

        // Past the hard timeout and no fresh cache entry. Consult SSM.
        let result = self.query_ssm_stale(task, &task_id_hint, remote);

        // cache the result keyed by task id for the TTL window.
        if !task_id_hint.is_empty() {
            if let Ok(mut cache) = self.ssm_stale_cache.lock() {
                cache.insert(task_id_hint, (now_secs, result));
            }
        }
        result
    }

    /// the SSM round-trip half of `is_task_stale`, factored out
    /// so the caller can cache the boolean result. Returns true when
    /// the task should be considered stale (reset to Ready), false
    /// when the SSM invocation is alive.
    fn query_ssm_stale(
        &self,
        _task: &Task,
        task_id_hint: &str,
        remote: &Option<ecaa_workflow_core::dag::RemoteExecution>,
    ) -> bool {
        let instance_id = remote
            .as_ref()
            .map(|r| r.instance_id.clone())
            .or_else(|| self.instance.as_ref().map(|i| i.instance_id.clone()))
            .unwrap_or_default();
        if instance_id.is_empty() {
            // No instance to query — conservative: consider stale so
            // the harness resets and the next iteration reprovisions.
            return true;
        }

        let comment = if task_id_hint.is_empty() {
            String::new()
        } else {
            format!("scripps-task-{}", task_id_hint)
        };
        let mut args: Vec<String> = vec![
            "ssm".into(),
            "list-command-invocations".into(),
            "--instance-id".into(),
            instance_id.clone(),
            "--details".into(),
            "--output".into(),
            "json".into(),
        ];
        if !comment.is_empty() {
            args.push("--filters".into());
            args.push(format!("Key=Comment,Values={}", comment));
        }
        let arg_refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        let stdout = match self.run_aws(&arg_refs) {
            Ok(v) => v,
            Err(_) => {
                // SSM unreachable → conservative reset so the task is
                // re-driven next iteration.
                return true;
            }
        };
        let v: serde_json::Value = match serde_json::from_str(&stdout) {
            Ok(v) => v,
            Err(_) => return true,
        };
        let invocations = v["CommandInvocations"].as_array();
        let Some(list) = invocations else {
            return true;
        };
        if list.is_empty() {
            // No matching invocation recorded for this task — instance
            // probably restarted / the command was never dispatched.
            return true;
        }
        // If ANY matching invocation is already terminal, the task is
        // not stale — the completion just hasn't been observed yet.
        // Fresh InProgress/Pending past the timeout IS stale.
        let mut any_running = false;
        for inv in list {
            let status = inv["Status"].as_str().unwrap_or("");
            match status {
                "Success" | "Failed" | "Cancelled" | "TimedOut" => return false,
                "InProgress" | "Pending" | "Delayed" => any_running = true,
                _ => {}
            }
        }
        any_running
    }

    /// Batched staleness check across multiple tasks that share one
    /// instance. AWS SSM `list-command-invocations` accepts comma-
    /// separated values in `--filters Key=Comment,Values=...`, so N
    /// per-task round-trips collapse to one. Returns a map keyed by
    /// task id; the verdict for each task is identical to what
    /// `query_ssm_stale` would have computed for it alone:
    ///
    /// * any matching invocation terminal (Success/Failed/Cancelled/
    ///   TimedOut) → false (not stale).
    /// * any matching invocation InProgress/Pending/Delayed (and no
    ///   terminal one) → true (stale).
    /// * no matching invocation → true (conservative: instance
    ///   probably restarted, treat as stale).
    ///
    /// Errors (SSM unreachable, JSON parse failure) map every requested
    /// id to true so the harness resets + reprovisions; matches the
    /// per-task version's failure mode.
    pub(super) fn query_ssm_stale_batched(
        &self,
        instance_id: &str,
        task_id_hints: &[String],
    ) -> std::collections::BTreeMap<String, bool> {
        let mut out: std::collections::BTreeMap<String, bool> = std::collections::BTreeMap::new();
        if instance_id.is_empty() || task_id_hints.is_empty() {
            for t in task_id_hints {
                out.insert(t.clone(), true);
            }
            return out;
        }
        let comment_values = task_id_hints
            .iter()
            .map(|t| format!("scripps-task-{}", t))
            .collect::<Vec<_>>()
            .join(",");
        let filter_arg = format!("Key=Comment,Values={}", comment_values);
        let args: Vec<&str> = vec![
            "ssm",
            "list-command-invocations",
            "--instance-id",
            instance_id,
            "--details",
            "--output",
            "json",
            "--filters",
            &filter_arg,
        ];
        let stdout = match self.run_aws(&args) {
            Ok(v) => v,
            Err(_) => {
                // SSM unreachable → conservative reset for every task.
                for t in task_id_hints {
                    out.insert(t.clone(), true);
                }
                return out;
            }
        };
        let v: serde_json::Value = match serde_json::from_str(&stdout) {
            Ok(v) => v,
            Err(_) => {
                for t in task_id_hints {
                    out.insert(t.clone(), true);
                }
                return out;
            }
        };
        let list = match v["CommandInvocations"].as_array() {
            Some(l) => l,
            None => {
                for t in task_id_hints {
                    out.insert(t.clone(), true);
                }
                return out;
            }
        };
        // Bucket invocations by the task-id derived from the Comment
        // field (`scripps-task-<task_id>`). Tasks with no invocation in
        // the response default to stale (matches per-task verdict).
        let mut by_task: std::collections::BTreeMap<String, (bool, bool)> =
            std::collections::BTreeMap::new();
        for t in task_id_hints {
            by_task.insert(t.clone(), (false, false));
        }
        for inv in list {
            let comment = inv["Comment"].as_str().unwrap_or("");
            let Some(tid) = comment.strip_prefix("scripps-task-") else {
                continue;
            };
            let status = inv["Status"].as_str().unwrap_or("");
            let entry = by_task.entry(tid.to_string()).or_insert((false, false));
            match status {
                "Success" | "Failed" | "Cancelled" | "TimedOut" => entry.0 = true,
                "InProgress" | "Pending" | "Delayed" => entry.1 = true,
                _ => {}
            }
        }
        for t in task_id_hints {
            let (has_terminal, any_running) = by_task.get(t).copied().unwrap_or((false, false));
            let stale = if has_terminal {
                false
            } else if any_running {
                true
            } else {
                // No invocation matched at all → conservative stale.
                true
            };
            out.insert(t.clone(), stale);
        }
        out
    }

    /// Batched staleness check + cache population. Calls
    /// `query_ssm_stale_batched` once for all tasks sharing the
    /// `instance_id`, then writes the per-task verdict into
    /// `ssm_stale_cache` so subsequent per-task `do_is_task_stale`
    /// calls return immediately from the cache without issuing
    /// additional SSM round-trips. Returns the batched verdict map for
    /// callers that need the results immediately.
    ///
    /// Caller contract: pass only tasks whose timestamp gate already
    /// exceeded the per-stage SSM timeout — pre-timeout tasks should
    /// short-circuit via the fast path in `do_is_task_stale` rather
    /// than burning the batched round-trip.
    pub fn prefill_stale_cache_batched(
        &self,
        instance_id: &str,
        task_id_hints: &[String],
        now_secs: u64,
    ) -> std::collections::BTreeMap<String, bool> {
        let verdicts = self.query_ssm_stale_batched(instance_id, task_id_hints);
        if let Ok(mut cache) = self.ssm_stale_cache.lock() {
            for (tid, stale) in &verdicts {
                cache.insert(tid.clone(), (now_secs, *stale));
            }
        }
        verdicts
    }
}

/// Look up the SSM timeout for a stage by loading the compute-resource
/// policy file that the emitter dropped into the package. Falls back to
/// the env var + 3600s default chain defined by
/// `sizing::resolve_ssm_timeout_secs`.
pub(super) fn resolve_ssm_timeout_for_stage(package: &str, stage_class: &str) -> u64 {
    let profiles_path = std::path::Path::new(package).join("policies/compute-resource-policy.json");
    let profiles = if profiles_path.exists() {
        match std::fs::read_to_string(&profiles_path)
            .ok()
            .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok())
            .and_then(|v| serde_yml::to_string(&v).ok())
            .and_then(|yaml| serde_yml::from_str::<ComputeProfiles>(&yaml).ok())
        {
            Some(p) => p,
            None => return fallback_ssm_timeout(),
        }
    } else {
        return fallback_ssm_timeout();
    };
    resolve_ssm_timeout_secs(&profiles, stage_class)
}

fn fallback_ssm_timeout() -> u64 {
    if let Ok(raw) = std::env::var("ECAA_AWS_SSM_TIMEOUT_SECS") {
        if let Ok(n) = raw.trim().parse::<u64>() {
            if n > 0 {
                return n;
            }
        }
    }
    3600
}

/// Clip a captured stream to its last `max_bytes` chars for safe
/// storage in WORKFLOW.json. SSM can return megabytes of stdout;
/// we only need the tail for result inspection.
fn tail_bytes(s: &str, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        return s.to_string();
    }
    let start = s.len().saturating_sub(max_bytes);
    // Snap to a char boundary so we don't corrupt UTF-8.
    let mut boundary = start;
    while boundary < s.len() && !s.is_char_boundary(boundary) {
        boundary += 1;
    }
    s[boundary..].to_string()
}

#[cfg(test)]
mod secret_filter_tests {
    use super::filter_secrets;
    use std::collections::BTreeMap;

    #[test]
    fn passes_non_secret_keys_unchanged() {
        let mut env = BTreeMap::new();
        env.insert("ECAA_HW_TOOL_THREAD_CURVES".to_string(), "{}".to_string());
        env.insert("ECAA_LIB_PIN_BWA".to_string(), "0.7.17".to_string());
        let (safe, dropped) = filter_secrets(&env);
        assert!(dropped.is_empty());
        assert_eq!(safe.len(), 2);
        assert_eq!(
            safe.get("ECAA_HW_TOOL_THREAD_CURVES").map(String::as_str),
            Some("{}")
        );
    }

    #[test]
    fn strips_anthropic_api_key_both_namespaces() {
        let mut env = BTreeMap::new();
        env.insert("ANTHROPIC_API_KEY".into(), "sk-secret".into());
        env.insert("ECAA_ANTHROPIC_API_KEY".into(), "sk-secret-2".into());
        env.insert("SAFE".into(), "value".into());
        let (safe, dropped) = filter_secrets(&env);
        assert_eq!(safe.len(), 1);
        assert!(safe.contains_key("SAFE"));
        assert!(dropped.contains(&"ANTHROPIC_API_KEY"));
        assert!(dropped.contains(&"ECAA_ANTHROPIC_API_KEY"));
    }

    #[test]
    fn strips_aws_credential_triple() {
        let mut env = BTreeMap::new();
        env.insert("AWS_ACCESS_KEY_ID".into(), "AKIA…".into());
        env.insert("AWS_SECRET_ACCESS_KEY".into(), "secret".into());
        env.insert("AWS_SESSION_TOKEN".into(), "tok".into());
        let (safe, dropped) = filter_secrets(&env);
        assert!(safe.is_empty());
        assert_eq!(dropped.len(), 3);
    }

    #[test]
    fn strips_github_tokens_and_other_known_secrets() {
        let mut env = BTreeMap::new();
        env.insert("GITHUB_TOKEN".into(), "ghp_x".into());
        env.insert("GH_TOKEN".into(), "ghp_y".into());
        env.insert("HF_TOKEN".into(), "hf_z".into());
        env.insert("ECAA_LIT_NCBI_API_KEY".into(), "k".into());
        let (safe, dropped) = filter_secrets(&env);
        assert!(safe.is_empty());
        assert_eq!(dropped.len(), 4);
    }

    #[test]
    fn empty_envelope_no_action() {
        let env: BTreeMap<String, String> = BTreeMap::new();
        let (safe, dropped) = filter_secrets(&env);
        assert!(safe.is_empty());
        assert!(dropped.is_empty());
    }

    /// Verifies that naming-convention variants are caught by the pattern
    /// matcher and that common non-secret vars survive the filter.
    #[test]
    fn filter_secrets_pattern_catches_naming_variants() {
        let mut env = BTreeMap::new();
        // Must be filtered — curated exact match (existing behavior).
        env.insert("GITHUB_TOKEN".into(), "ghp_a".into());
        // Must be filtered — suffix variant not in curated list (new behavior).
        env.insert("GITHUB_TOKEN_BACKUP".into(), "ghp_b".into());
        // Must be filtered — prefix + _SECRET pattern (new behavior).
        env.insert("MY_STRIPE_SECRET".into(), "sk_live_x".into());
        // Must be filtered — curated exact match (ANTHROPIC_API_KEY).
        env.insert("ANTHROPIC_API_KEY".into(), "sk-ant".into());
        // Must NOT be filtered — well-known safe PATH variable.
        env.insert("PATH".into(), "/usr/bin:/bin".into());
        // Must NOT be filtered — contains "PATH" but not a secret pattern.
        env.insert("LD_LIBRARY_PATH".into(), "/usr/lib".into());
        // Must NOT be filtered — log-level config, no secret suffix.
        env.insert("RUST_LOG".into(), "info".into());

        let (safe, dropped) = filter_secrets(&env);

        assert!(
            dropped.iter().any(|k| *k == "GITHUB_TOKEN"),
            "GITHUB_TOKEN must be in dropped list (curated)"
        );
        // GITHUB_TOKEN_BACKUP and MY_STRIPE_SECRET are pattern-matched;
        // they appear as the sentinel string in `dropped`.
        let sentinel_count = dropped
            .iter()
            .filter(|k| **k == "<pattern-matched secret>")
            .count();
        assert!(
            sentinel_count >= 2,
            "expected at least 2 pattern-matched secrets, got {}",
            sentinel_count
        );
        assert!(
            dropped.iter().any(|k| *k == "ANTHROPIC_API_KEY"),
            "ANTHROPIC_API_KEY must be in dropped list (curated)"
        );

        assert!(safe.contains_key("PATH"), "PATH must pass filter");
        assert!(
            safe.contains_key("LD_LIBRARY_PATH"),
            "LD_LIBRARY_PATH must pass filter"
        );
        assert!(safe.contains_key("RUST_LOG"), "RUST_LOG must pass filter");

        // The three safe vars and no secret vars.
        assert_eq!(
            safe.len(),
            3,
            "exactly PATH, LD_LIBRARY_PATH, RUST_LOG should survive"
        );
    }
}
