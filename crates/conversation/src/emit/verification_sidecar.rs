//! Per-task verification sidecar writer.
//!
//! At emit time, walks every task in the emitted DAG that has a
//! `Completed` state + a narrative artifact under
//! `runtime/<task_id>/` and runs the same claim-extractor +
//! claim-verifier pipeline the server's `GET /task/:task_id/result`
//! handler runs synchronously per poll. Persists each per-task report
//! at `<package>/runtime/verification-reports/<task_id>.json` so the
//! GET handler can read the sidecar instead of re-verifying on every
//! request.
//!
//! Gated by the package's `policies/interpretation-policy.json`:
//! only fires when a `verifiableEntities.enabled` block is true. This
//! mirrors the live-verification gate at `get_task_result`.
//!
//! Failures are non-fatal: a missing or unreadable sidecar means the
//! GET handler falls back to live verification.

use crate::session::Session;
use std::path::Path;

/// Walk every task in the emitted package's `WORKFLOW.json` that has a
/// Completed status and run `verify_task_with_context_emit_time`,
/// persisting the per-task report at
/// `<package>/runtime/verification-reports/<task_id>.json`. The
/// `/task/:task_id/result` GET handler reads these instead of re-running
/// verification on every poll.
///
/// Only fires when the package's `policies/interpretation-policy.json`
/// declares a `verifiableEntities.enabled: true` block, matching the
/// existing live-verification gate at `get_task_result`.
///
/// Failures (unreadable policy, missing WORKFLOW.json, malformed task
/// records, individual verification errors) are all swallowed
/// best-effort: the GET handler falls back to live verification when a
/// sidecar is absent.
pub(crate) fn write_verification_sidecars(
    package_root: &Path,
    session: &Session,
    config_dir: &Path,
) -> anyhow::Result<()> {
    // Package-side policy gate. Mirrors the GET handler's behavior of
    // skipping verification when the policy lacks a `verifiableEntities`
    // block. An unreadable / malformed policy degrades to "no sidecars"
    // so the GET handler falls back to live verification.
    let policy_path = package_root
        .join("policies")
        .join("interpretation-policy.json");
    let policy: serde_json::Value = match std::fs::read_to_string(&policy_path) {
        Ok(s) => match serde_json::from_str(&s) {
            Ok(v) => v,
            Err(_) => return Ok(()),
        },
        Err(_) => return Ok(()),
    };
    let enabled = policy
        .get("verifiableEntities")
        .and_then(|v| v.get("enabled"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    if !enabled {
        return Ok(());
    }

    let sidecar_dir = package_root.join("runtime").join("verification-reports");
    let _ = std::fs::create_dir_all(&sidecar_dir);

    // Iterate tasks from the on-disk WORKFLOW.json (the emit just wrote
    // it). The `tasks` shape is an object keyed by task_id; each value
    // carries a `state.status` discriminant string.
    let workflow_json_path = package_root.join("WORKFLOW.json");
    let workflow_bytes = match std::fs::read_to_string(&workflow_json_path) {
        Ok(s) => s,
        Err(_) => return Ok(()),
    };
    let workflow: serde_json::Value = match serde_json::from_str(&workflow_bytes) {
        Ok(v) => v,
        Err(_) => return Ok(()),
    };
    let tasks = match workflow.get("tasks").and_then(|t| t.as_object()) {
        Some(t) => t,
        None => return Ok(()),
    };

    for (task_id, task) in tasks {
        let status = task
            .get("state")
            .and_then(|s| s.get("status"))
            .and_then(|s| s.as_str())
            .unwrap_or("");
        if !status.eq_ignore_ascii_case("completed") {
            continue;
        }
        if let Some(report) =
            scripps_workflow_core::claim_verifier::verify_task_with_context_emit_time(
                package_root,
                task_id,
                config_dir,
                session.project_class,
                &session.decisions,
                session.mode.is_confirmatory(),
            )
        {
            if let Ok(bytes) = serde_json::to_vec_pretty(&report) {
                let sidecar = sidecar_dir.join(format!("{task_id}.json"));
                let tmp = sidecar.with_extension("json.tmp");
                let _ = std::fs::write(&tmp, &bytes).and_then(|_| std::fs::rename(&tmp, &sidecar));
            }
        }
    }
    Ok(())
}
