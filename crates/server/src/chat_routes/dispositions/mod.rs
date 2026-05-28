//! Disposition auto-apply plan §7.2 — server ingest of agent-written
//! `sme_disposition.json` files + REST surface for the UI's
//! DispositionReviewCard.
//!
//! Split out of the previous `dispositions.rs` (1590 LOC)
//! into this directory: `scan.rs` (read paths + ingest triggers),
//! `apply.rs` (POST apply + apply-one), `review.rs` (POST reject). This
//! file owns the shared helpers (`load_disposition`, `persist_disposition`,
//! queue I/O, decision recorders) and the canonical `apply_actions`
//! engine that both `scan.rs` (auto-apply trigger) and `apply.rs`
//! (handler) call into.
//!
//! The module owns:
//!
//! - `DispositionQueue` — per-session in-memory index of pending +
//!   recently-applied dispositions. Backed by the file on disk; the
//!   queue is restart-rebuilt via `POST /dispositions/scan`.
//! - Detection trigger (1) — `ingest_disposition_from_progress_event`
//!   hooks the `post_progress` handler when the harness forwards an
//!   agent-written `disposition_proposed` event.
//! - Detection trigger (2) — `scan_after_task_completed` hooks the
//!   `task_completed` progress handler (§7.4).
//! - Detection trigger (3) — `scan_session_dispositions` rebuilds the
//!   queue from disk; also powers `POST /dispositions/scan`.
//! - Apply/reject flow — serially calls the existing
//!   `amend_stage_method_from_rest` / `rerun_task_from_rest` service
//!   methods, writing one `DispositionApplied` / `DispositionRejected`
//!   decision record per action.
//! - v2 escape hatch — `auto_apply_if_enabled` is fired on ingest and
//!   checks `ECAA_AUTO_APPLY_DISPOSITIONS=1 && disposition.auto_apply`.

use super::*;
use ecaa_workflow_core::decision_log::{DecisionActor, DecisionType};
use ecaa_workflow_core::disposition::{
    normalize, Action, Disposition, DispositionStatus, DISPOSITION_SCHEMA_VERSION,
};
use serde::Serialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::RwLock;

mod apply;
mod review;
mod scan;

// Public re-exports — preserve the public API previously offered by
// the flat `dispositions.rs` so `events.rs` + `app_state.rs` keep
// compiling unchanged.
pub(crate) use scan::{ingest_disposition_from_progress_event, scan_after_task_completed};

/// Per-session cache of normalized dispositions keyed by their
/// relative path inside the emitted package
/// (`runtime/outputs/<task>/sme_disposition.json`). Backed by the file
/// on disk — the queue is a UX-time index, not the durable store.
pub(crate) type DispositionQueue = Arc<RwLock<BTreeMap<SessionId, BTreeMap<PathBuf, Disposition>>>>;

/// Return true when the env var `ECAA_AUTO_APPLY_DISPOSITIONS=1` is
/// set. The double-gate (env + per-file `auto_apply: true`) means both
/// operator consent and agent consent are required before the server
/// applies without an SME click.
pub(crate) fn auto_apply_enabled_globally() -> bool {
    ecaa_workflow_core::env_helpers::env_bool("ECAA_AUTO_APPLY_DISPOSITIONS")
}

/// Load + normalize a disposition file on disk. Returns Err with a
/// human-readable reason when the file is missing, unparseable, or
/// missing required fields. Path is relative to the emitted package
/// root so the server never has to trust an absolute path supplied by
/// the client.
pub(super) fn load_disposition(pkg: &Path, rel_path: &Path) -> Result<Disposition, String> {
    let full = pkg.join(rel_path);
    // Guard against path traversal — canonicalize both sides, then
    // verify the resolved path stays inside the package root.
    let Ok(root_canon) = pkg.canonicalize() else {
        return Err(format!(
            "package root {} not canonicalizable",
            pkg.display()
        ));
    };
    let Ok(full_canon) = full.canonicalize() else {
        return Err(format!("disposition file {} not found", full.display()));
    };
    if !full_canon.starts_with(&root_canon) {
        return Err("disposition path escapes the emitted package root".to_string());
    }
    let bytes = std::fs::read(&full_canon).map_err(|e| format!("read: {}", e))?;
    let raw: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|e| format!("parse: {}", e))?;
    normalize(raw)
}

/// Write the disposition back to disk with updated status +
/// status_updated_at. Atomic `.tmp` + rename so readers never observe
/// a partial file. Best-effort — logs on failure but never propagates
/// the error upstream (the mutation has already happened; the on-disk
/// status is advisory for audit purposes).
pub(super) fn persist_disposition(pkg: &Path, rel_path: &Path, d: &Disposition) {
    // Reject `..`-bearing rel_paths up front. `rel_path` arrives from
    // URL via `decode_path` which doesn't validate traversal — only
    // `load_disposition`'s canonicalize check protects reads, so writes
    // need their own guard before joining against the package root.
    if rel_path
        .components()
        .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        eprintln!(
            "[disposition_persist] rejecting traversal in rel_path {}",
            rel_path.display()
        );
        return;
    }
    let full = pkg.join(rel_path);
    if let Err(e) = super::_path_jail::assert_under_root(pkg, &full) {
        eprintln!("[disposition_persist] rel_path escapes package root: {}", e);
        return;
    }
    let Some(parent) = full.parent() else {
        eprintln!("[disposition_persist] no parent for {}", full.display());
        return;
    };
    let tmp = parent.join(format!(
        ".{}.tmp",
        full.file_name()
            .and_then(|f| f.to_str())
            .unwrap_or("sme_disposition.json")
    ));
    let serialized = match serde_json::to_vec_pretty(d) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("[disposition_persist] serialize failed: {}", e);
            return;
        }
    };
    if let Err(e) = std::fs::write(&tmp, &serialized) {
        eprintln!("[disposition_persist] write tmp failed: {}", e);
        return;
    }
    if let Err(e) = std::fs::rename(&tmp, &full) {
        eprintln!("[disposition_persist] rename failed: {}", e);
    }
}

/// Walk a session's emitted package for `runtime/outputs/*/sme_disposition.json`
/// and load each one. Used by the scan endpoint and the task_completed
/// backfill hook. Returns a map keyed by relative path.
pub(super) fn scan_disk(pkg: &Path) -> BTreeMap<PathBuf, Disposition> {
    let mut out = BTreeMap::new();
    let outputs_dir = pkg.join("runtime").join("outputs");
    let Ok(entries) = std::fs::read_dir(&outputs_dir) else {
        return out;
    };
    for entry in entries.flatten() {
        let task_dir = entry.path();
        if !task_dir.is_dir() {
            continue;
        }
        let disposition_path = task_dir.join("sme_disposition.json");
        if !disposition_path.exists() {
            continue;
        }
        let Some(task_name) = task_dir.file_name().and_then(|f| f.to_str()) else {
            continue;
        };
        let rel_path = PathBuf::from("runtime/outputs")
            .join(task_name)
            .join("sme_disposition.json");
        match load_disposition(pkg, &rel_path) {
            Ok(d) => {
                out.insert(rel_path, d);
            }
            Err(e) => {
                eprintln!(
                    "[disposition_scan] skipping {}: {}",
                    disposition_path.display(),
                    e
                );
            }
        }
    }
    out
}

/// Insert one disposition into the in-memory queue without re-reading
/// disk. Used by the ingest hook after normalization.
pub(super) async fn queue_upsert(
    queue: &DispositionQueue,
    session_id: SessionId,
    rel_path: PathBuf,
    d: Disposition,
) {
    let mut guard = queue.write().await;
    let entry = guard.entry(session_id).or_default();
    entry.insert(rel_path, d);
}

/// Look up one disposition in the queue; falls back to disk load on a
/// cache miss (e.g. after a server restart before the UI has called
/// `/dispositions/scan`).
pub(super) async fn queue_get_or_load(
    queue: &DispositionQueue,
    session_id: SessionId,
    pkg: &Path,
    rel_path: &Path,
) -> Option<Disposition> {
    {
        let guard = queue.read().await;
        if let Some(session_map) = guard.get(&session_id) {
            if let Some(d) = session_map.get(rel_path) {
                return Some(d.clone());
            }
        }
    }
    // Cache miss — try to load from disk.
    match load_disposition(pkg, rel_path) {
        Ok(d) => {
            queue_upsert(queue, session_id, rel_path.to_path_buf(), d.clone()).await;
            Some(d)
        }
        Err(e) => {
            // Demoted from
            // `eprintln!` so the session-id surface stays in the
            // structured tracing pipeline alongside the http span.
            tracing::warn!(
                ?session_id,
                path = %rel_path.display(),
                error = %e,
                "disposition_load failed"
            );
            None
        }
    }
}

pub(super) async fn record_proposed_decision(
    app: &ChatAppState,
    session_id: SessionId,
    rel_path: &Path,
    d: &Disposition,
) {
    let path_str = rel_path.to_string_lossy().to_string();
    let created_at = d
        .created_at
        .clone()
        .unwrap_or_else(ecaa_workflow_core::time_helpers::now_rfc3339);
    let task_id = d.task_id.clone();
    let action_count = d.actions.len();
    let _ = app
        .conversation
        .store_handle()
        .update(session_id, move |s| {
            s.record_decision(
                DecisionType::DispositionProposed {
                    path: path_str.clone(),
                    task_id: task_id.clone(),
                    created_at: created_at.clone(),
                    action_count,
                },
                DecisionActor::Llm,
                None,
            );
            Ok(())
        })
        .await;
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn record_applied_decision(
    app: &ChatAppState,
    session_id: SessionId,
    rel_path: &Path,
    action_index: usize,
    action_kind: &str,
    target_stage: &str,
    outcome: &str,
    error_reason: Option<String>,
    auto: bool,
) {
    let rel = rel_path.to_string_lossy().to_string();
    let kind = action_kind.to_string();
    let target = target_stage.to_string();
    let outcome_s = outcome.to_string();
    let _ = app
        .conversation
        .store_handle()
        .update(session_id, move |s| {
            s.record_decision(
                DecisionType::DispositionApplied {
                    path: rel.clone(),
                    action_index,
                    action_kind: kind.clone(),
                    target_stage: target.clone(),
                    outcome: outcome_s.clone(),
                    error_reason: error_reason.clone(),
                    auto,
                },
                if auto {
                    DecisionActor::Harness
                } else {
                    DecisionActor::Sme
                },
                None,
            );
            Ok(())
        })
        .await;
}

pub(super) async fn record_rejected_decision(
    app: &ChatAppState,
    session_id: SessionId,
    rel_path: &Path,
    rationale: Option<String>,
) {
    let rel = rel_path.to_string_lossy().to_string();
    let _ = app
        .conversation
        .store_handle()
        .update(session_id, move |s| {
            s.record_decision(
                DecisionType::DispositionRejected {
                    path: rel.clone(),
                    rationale: rationale.clone(),
                },
                DecisionActor::Sme,
                rationale.clone(),
            );
            Ok(())
        })
        .await;
}

pub(super) fn decode_path(raw: &str) -> PathBuf {
    // `axum` wildcard captures leave the path unencoded per-segment but
    // the extractor gives us the raw slice after the prefix. Just trim
    // a leading slash and pass through — the canonicalize check inside
    // `load_disposition` blocks any traversal attempt.
    let trimmed = raw.trim_start_matches('/');
    PathBuf::from(trimmed)
}

/// Outcome of a single-action apply. `invalidated_tasks` is the
/// union across every successful action — callers pass this to the
/// UI so it can render "N stages will re-run" badges.
#[derive(Debug, Serialize, Default)]
pub(crate) struct ApplyOutcome {
    pub applied: usize,
    pub failed: usize,
    pub invalidated_tasks: Vec<String>,
    pub status: DispositionStatus,
    pub errors: Vec<ApplyError>,
}

#[derive(Debug, Serialize)]
pub(crate) struct ApplyError {
    pub action_index: usize,
    pub action_kind: String,
    pub target_stage: String,
    pub reason: String,
}

/// Serially apply every action in the disposition's canonical
/// `actions[]` list. Follows the plan §5.2 flow: for each action,
/// match kind, dispatch to the existing REST-equivalent service
/// helper, record a `DispositionApplied` decision, and stop on the
/// first error. Updates the disposition's on-disk status to `applied`
/// or `partial` before returning. Fires one `maybe_auto_relaunch_harness`
/// call at the end so the harness resumes against the amended DAG
/// without requiring a separate Resume click.
pub(crate) async fn apply_actions(
    app: &ChatAppState,
    session_id: SessionId,
    pkg: &Path,
    rel_path: &Path,
    auto: bool,
) -> ApplyOutcome {
    let Some(mut d) = queue_get_or_load(&app.dispositions, session_id, pkg, rel_path).await else {
        return ApplyOutcome {
            applied: 0,
            failed: 0,
            invalidated_tasks: vec![],
            status: DispositionStatus::Pending,
            errors: vec![ApplyError {
                action_index: 0,
                action_kind: "-".into(),
                target_stage: "-".into(),
                reason: "disposition not found".into(),
            }],
        };
    };
    if !matches!(
        d.status,
        DispositionStatus::Pending | DispositionStatus::Partial
    ) {
        // Idempotent — nothing to do on a file already marked applied
        // or rejected. Short-circuit so a double-click doesn't
        // re-invoke the mutations.
        return ApplyOutcome {
            applied: 0,
            failed: 0,
            invalidated_tasks: vec![],
            status: d.status,
            errors: vec![],
        };
    }
    let mut invalidated_union: Vec<String> = Vec::new();
    let mut applied = 0usize;
    let mut failed = 0usize;
    let mut errors: Vec<ApplyError> = Vec::new();
    for (idx, action) in d.actions.iter().enumerate() {
        let (kind_label, target_label, result) = match action {
            Action::AmendMethod {
                target_stage,
                new_method,
                rationale,
                ..
            } => {
                let r = app
                    .conversation
                    .amend_stage_method_from_rest(
                        session_id,
                        target_stage.clone(),
                        new_method.clone(),
                        rationale.clone(),
                    )
                    .await
                    .map(|res| res.invalidated_tasks);
                (
                    "amend_method".to_string(),
                    target_stage.clone(),
                    r.map_err(|e| format!("{}", e)),
                )
            }
            Action::Rerun {
                target_stage,
                reason,
            } => {
                let r = app
                    .conversation
                    .rerun_task_from_rest(session_id, target_stage.clone(), reason.clone())
                    .await;
                (
                    "rerun".to_string(),
                    target_stage.clone(),
                    r.map_err(|e| format!("{}", e)),
                )
            }
            Action::InvalidateSlice { from_stage, .. } => {
                // Advisory — the preceding amend already invalidated
                // the forward slice. Record a successful apply so the
                // audit log still has one entry per action.
                (
                    "invalidate_slice".to_string(),
                    from_stage.clone(),
                    Ok(Vec::new()),
                )
            }
            Action::PreservePin {
                target_stage,
                method: _,
            } => {
                // v2 — not wired into the apply loop. Record an OK
                // (skipped) entry so the log is honest. See plan §4.1.
                (
                    "preserve_pin".to_string(),
                    target_stage.clone(),
                    Ok(Vec::new()),
                )
            }
        };
        match result {
            Ok(invalidated) => {
                applied += 1;
                for t in invalidated {
                    if !invalidated_union.contains(&t) {
                        invalidated_union.push(t);
                    }
                }
                record_applied_decision(
                    app,
                    session_id,
                    rel_path,
                    idx,
                    &kind_label,
                    &target_label,
                    "ok",
                    None,
                    auto,
                )
                .await;
            }
            Err(msg) => {
                failed += 1;
                errors.push(ApplyError {
                    action_index: idx,
                    action_kind: kind_label.clone(),
                    target_stage: target_label.clone(),
                    reason: msg.clone(),
                });
                record_applied_decision(
                    app,
                    session_id,
                    rel_path,
                    idx,
                    &kind_label,
                    &target_label,
                    "err",
                    Some(msg.clone()),
                    auto,
                )
                .await;
                // Stop on first error — plan §5.2.
                break;
            }
        }
    }
    d.status = if failed > 0 {
        DispositionStatus::Partial
    } else {
        DispositionStatus::Applied
    };
    d.status_updated_at = Some(ecaa_workflow_core::time_helpers::now_rfc3339());
    if d.schema_version == 0 {
        d.schema_version = DISPOSITION_SCHEMA_VERSION;
    }
    // Write the status back to disk + refresh the in-memory queue.
    persist_disposition(pkg, rel_path, &d);
    queue_upsert(
        &app.dispositions,
        session_id,
        rel_path.to_path_buf(),
        d.clone(),
    )
    .await;
    // Single auto-relaunch call — plan §5.2 "fire auto_relaunch hook
    // (one call, not per-action)".
    if applied > 0 && failed == 0 {
        super::execution::maybe_auto_relaunch_harness(app, session_id, "disposition_apply").await;
    }
    ApplyOutcome {
        applied,
        failed,
        invalidated_tasks: invalidated_union,
        status: d.status,
        errors,
    }
}

/// Route inventory for the doc-as-contract gate +
/// per-submodule `routes()` builder. `mod.rs::router()` merges every
/// submodule's builder into the single chat surface.
pub(crate) const ROUTES: &[(&str, &str)] = &[
    ("GET", "/api/chat/session/:id/dispositions"),
    ("POST", "/api/chat/session/:id/dispositions/scan"),
    ("GET", "/api/chat/session/:id/dispositions/view/*path"),
    ("POST", "/api/chat/session/:id/dispositions/apply/*path"),
    ("POST", "/api/chat/session/:id/dispositions/apply-one/*path"),
    ("POST", "/api/chat/session/:id/dispositions/reject/*path"),
];

pub(crate) fn routes() -> axum::Router<ChatAppState> {
    axum::Router::new()
        .route(
            "/api/chat/session/:id/dispositions",
            axum::routing::get(scan::list_dispositions),
        )
        .route(
            "/api/chat/session/:id/dispositions/scan",
            axum::routing::post(scan::post_scan),
        )
        .route(
            "/api/chat/session/:id/dispositions/view/*path",
            axum::routing::get(scan::get_disposition),
        )
        .route(
            "/api/chat/session/:id/dispositions/apply/*path",
            axum::routing::post(apply::post_apply),
        )
        .route(
            "/api/chat/session/:id/dispositions/apply-one/*path",
            axum::routing::post(apply::post_apply_one),
        )
        .route(
            "/api/chat/session/:id/dispositions/reject/*path",
            axum::routing::post(review::post_reject),
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use ecaa_workflow_core::dag::TaskId;
    use ecaa_workflow_core::disposition::{
        Disposition as CoreDisposition, DispositionStatus as CoreStatus,
    };

    fn stub_disposition() -> CoreDisposition {
        CoreDisposition {
            schema_version: DISPOSITION_SCHEMA_VERSION,
            task_id: TaskId::from("legit"),
            created_at: Some("2026-05-14T00:00:00Z".into()),
            authoritative_interpretation: None,
            actions: vec![],
            auto_apply: false,
            status: CoreStatus::Pending,
            status_updated_at: None,
            legacy_passthrough: serde_json::Map::new(),
        }
    }

    #[test]
    fn persist_disposition_rejects_traversal_rel_path() {
        let tmp = tempfile::tempdir().unwrap();
        let pkg = tmp.path().join("pkg");
        std::fs::create_dir_all(pkg.join("runtime/outputs/legit")).unwrap();
        // A traversal rel_path must NOT produce a write outside pkg.
        let rel = PathBuf::from("../etc/sme_disposition.json");
        let d = stub_disposition();
        persist_disposition(&pkg, &rel, &d);
        // Confirm no file was written outside pkg (traversal blocked early).
        assert!(!tmp.path().join("etc/sme_disposition.json").exists());
    }
}
