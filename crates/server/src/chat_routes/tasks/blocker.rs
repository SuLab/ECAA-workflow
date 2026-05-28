//! Blocker-info read endpoint + SME response writers
//! (sme-decisions, sme-selection, auto-approve-discoveries).
//!
//! Endpoints (plan §S16.2):
//! - GET /session/:id/task/:task_id/blocker
//! - POST /session/:id/task/:task_id/sme-decisions
//! - POST /session/:id/task/:task_id/sme-selection
//! - POST /session/:id/auto-approve-discoveries
//!
//! All four emit auto-relaunch hooks (where applicable) and write
//! agent-readable artifacts under `runtime/outputs/<task_id>/`.

use super::super::*;
use axum::{extract::State, http::StatusCode, response::IntoResponse, Json};
use uuid::Uuid;

/// High-stakes discovery stages that ALWAYS block for SME review, even
/// when the auto-approve checkbox is checked. Any axis that ends up in
/// both `allow` and `deny` is blocked — defensive double-entry guards
/// against an unintended override if a future atom is added to the
/// registry without thinking about whether SME judgment is required.
///
/// The UI checkbox label promises "not integration, annotation, DE, or
/// validation" — this list realizes that promise plus a small set of
/// other stages where method choice is biologically load-bearing
/// (batch correction, pathway enrichment) and any `validate_*` stage is
/// rejected unconditionally by the harness regardless of marker
/// content (see `crates/harness/src/scheduler.rs::is_safe_stage_token`
/// gating + the agent prompt at task-execution.md §SCOPE).
const HIGH_STAKES_DISCOVER_AXES: &[&str] = &[
    "batch_correction",
    "cell_type_annotation",
    "differential_expression",
    "differential_accessibility",
    "differential_loop_analysis",
    "differential_transcript_usage",
    "pathway_enrichment",
    // Cross-omics integration axes — joint analysis decisions warrant
    // SME review even when the rest of the pipeline auto-approves.
    "joint_wnn_integration",
];

/// Fallback allowlist used only when the on-disk atom registry can't be
/// loaded (e.g. `SWFC_CONFIG_DIR` misconfigured, registry corrupted on
/// disk). Keeps the auto-approve checkbox useful in a degraded state
/// instead of writing an empty file the agent will treat as "block
/// everything." Kept tiny on purpose — the production path always goes
/// through `AtomRegistry::discover_axes`.
const FALLBACK_AUTO_APPROVE_ALLOW: &[&str] = &[
    "alignment",
    "sequence_trimming",
    "quantification",
    "normalisation",
    "dimensionality_reduction",
    "clustering",
    "peak_calling",
    "variant_calling",
];

/// Compute the auto-approve allow list for the BlockerCard checkbox.
///
/// Production path: walk the atom registry, take every discover axis
/// (mirrors `composer_v4::synthesize_discover_companions::derive_axis`),
/// remove the [`HIGH_STAKES_DISCOVER_AXES`] entries. Sorted for
/// byte-stable output.
///
/// Returns `(allow, deny)` for the marker file. `deny` is the
/// high-stakes list itself so the agent prompt's "deny ALWAYS blocks"
/// rule is enforced server-side as well.
fn compute_allow_deny(config_dir: &std::path::Path) -> (Vec<String>, Vec<String>) {
    let stage_atoms_dir = config_dir.join("stage-atoms");
    let allow_from_registry: Vec<String> =
        match ecaa_workflow_core::atom_registry::AtomRegistry::load_from_dir(&stage_atoms_dir) {
            Ok(reg) => reg
                .discover_axes()
                .into_iter()
                .filter(|axis| !HIGH_STAKES_DISCOVER_AXES.contains(&axis.as_str()))
                .collect(),
            Err(err) => {
                tracing::warn!(
                    error = %err,
                    path = %stage_atoms_dir.display(),
                    "auto_approve_discoveries: atom registry load failed; using fallback allowlist"
                );
                Vec::new()
            }
        };
    // Empty result (either registry missing on disk OR registry loaded
    // but contained zero discover atoms) must NEVER yield an empty allow
    // list — that would make the BlockerCard checkbox a no-op in the
    // degraded state too. Fall back to the tiny known-good list so
    // routine stages still auto-approve even when config_dir is
    // misconfigured.
    let allow: Vec<String> = if allow_from_registry.is_empty() {
        FALLBACK_AUTO_APPROVE_ALLOW
            .iter()
            .map(|s| (*s).to_string())
            .collect()
    } else {
        allow_from_registry
    };
    let deny: Vec<String> = HIGH_STAKES_DISCOVER_AXES
        .iter()
        .map(|s| (*s).to_string())
        .collect();
    (allow, deny)
}

/// `POST /api/chat/session/:id/auto-approve-discoveries` — write the auto-approve sentinel file.
pub async fn auto_approve_discoveries(
    State(app): State<ChatAppState>,
    Path(session_id): Path<Uuid>,
) -> impl IntoResponse {
    let Some(session) = app.conversation.get_session(session_id).await else {
        return (StatusCode::NOT_FOUND, "session not found").into_response();
    };
    let Some(pkg) = session.emitted_package_path.clone() else {
        return (StatusCode::BAD_REQUEST, "session has no emitted package").into_response();
    };
    let marker = pkg.join("runtime").join(".sme-auto-approve-discoveries");
    if let Some(parent) = marker.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let (allow, deny) = compute_allow_deny(&super::config_dir_or_default());
    let body = serde_json::json!({ "allow": allow, "deny": deny });
    match std::fs::write(
        &marker,
        serde_json::to_vec_pretty(&body).unwrap_or_default(),
    ) {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("failed to write marker: {}", e),
        )
            .into_response(),
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct SmeSelectionRequest {
    pub chosen: String,
}

/// SME picked a specific candidate (possibly a runner-up)
/// for a blocked discovery task. Writes
/// <package>/runtime/outputs/<task_id>/sme-selection.json so the agent
/// on resume completes with that candidate instead of the top pick.
#[tracing::instrument(skip(app, req), fields(session_id = %session_id, task_id = %task_id))]
pub async fn post_sme_selection(
    State(app): State<ChatAppState>,
    Path((session_id, task_id)): Path<(Uuid, String)>,
    Json(req): Json<SmeSelectionRequest>,
) -> impl IntoResponse {
    let Some(session) = app.conversation.get_session(session_id).await else {
        return (StatusCode::NOT_FOUND, "session not found").into_response();
    };
    let Some(pkg) = session.emitted_package_path.clone() else {
        return (StatusCode::BAD_REQUEST, "session has no emitted package").into_response();
    };
    // Route through `runtime_outputs_for_task` so every task_id-bearing
    // handler shares the same centralized path-jail / traversal-rejection
    // logic.
    let out_dir = match super::super::_path_jail::runtime_outputs_for_task(&pkg, &task_id) {
        Ok(p) => p,
        Err(e) => {
            return (StatusCode::BAD_REQUEST, format!("invalid task_id: {}", e)).into_response();
        }
    };
    if let Err(e) = std::fs::create_dir_all(&out_dir) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("failed to create output dir: {}", e),
        )
            .into_response();
    }
    let path = out_dir.join("sme-selection.json");
    // Belt-and-suspenders canonicalize check now that the parent
    // exists. Rejects symlink-based escapes that segment-only checks
    // would miss.
    if let Err(e) = super::super::assert_under_root(&pkg, &path) {
        return (StatusCode::FORBIDDEN, format!("path escapes root: {}", e)).into_response();
    }
    let body = serde_json::json!({
        "chosen": req.chosen,
        "timestamp": ecaa_workflow_core::time_helpers::now_rfc3339(),
    });
    if let Err(e) = std::fs::write(&path, serde_json::to_vec_pretty(&body).unwrap_or_default()) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("failed to write selection: {}", e),
        )
            .into_response();
    }

    // Picking a candidate IS the SME's review-gate confirmation for the
    // discover_* task. Without a `runtime/sme-review-confirmed-<task_id>.json`
    // sidecar the harness scheduler's `filter_picks_respecting_sme_gate` keeps
    // every downstream task pinned at Ready forever — the agent completes the
    // discover task with the picked method, but the dispatcher refuses to
    // walk past the (now-completed-but-unconfirmed) review gate. Mirror the
    // /confirm-stage write so a single click both records the choice and
    // opens the gate.
    let runtime_dir = pkg.join("runtime");
    let sidecar_name = format!("sme-review-confirmed-{}.json", task_id);
    let sidecar_path = match super::super::safe_segment_join(&runtime_dir, &sidecar_name) {
        Ok(p) => p,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                format!("invalid task_id for sidecar: {}", e),
            )
                .into_response();
        }
    };
    if let Err(e) = std::fs::create_dir_all(&runtime_dir) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!(
                "creating runtime dir for sme-review-confirmed sidecar: {}",
                e
            ),
        )
            .into_response();
    }
    if let Err(e) = super::super::assert_under_root(&pkg, &sidecar_path) {
        return (
            StatusCode::FORBIDDEN,
            format!("sme-review-confirmed sidecar path escapes root: {}", e),
        )
            .into_response();
    }
    let sidecar_body = serde_json::json!({
        "stage": task_id,
        "confirmed_at": ecaa_workflow_core::time_helpers::now_rfc3339(),
        "via": "sme_selection",
        "chosen": req.chosen,
    });
    if let Err(e) = std::fs::write(
        &sidecar_path,
        serde_json::to_vec_pretty(&sidecar_body).unwrap_or_default(),
    ) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("writing sme-review-confirmed sidecar: {}", e),
        )
            .into_response();
    }

    // Resume blocked tasks in WORKFLOW.json before auto-relaunch — without
    // this the picked task stays Blocked, the auto-relaunch predicate sees
    // no ready task, and the harness sleeps. The SME's mental model is
    // "I picked a method → the task resumes"; requiring a separate
    // /unblock call to clear the blocker surfaces as "I clicked the
    // BlockerCard button but nothing happened". Mirrors the
    // sme_decisions / unblock handlers so a single click both records
    // the pick and re-arms the DAG.
    if let Err(e) = super::super::execution::resume_blocked_tasks_in_workflow(&pkg).await {
        eprintln!(
            "[sme_selection] WORKFLOW.json resume failed for {}: {}",
            pkg.display(),
            e
        );
    }

    // Auto-relaunch hook. Selecting a candidate
    // is a "resume execution now" signal. The decision predicate
    // inside maybe_auto_relaunch_harness enforces the
    // has-ready-task guard so a premature spawn is harmless.
    super::super::execution::maybe_auto_relaunch_harness(&app, session_id, "sme_selection").await;
    StatusCode::NO_CONTENT.into_response()
}

/// Serve the agent-written `runtime/outputs/<task_id>/blocker.json`
/// so the BlockerCard can render a structured decision picker when
/// the blocker is of kind `AwaitingStructuredDecision`,
/// `RuntimeCapabilityMissing`, or `AwaitingSmeApproval`. Response
/// shape: `{blocker: <json|null>}` — null when the file is absent,
/// so the UI can render "agent hasn't yet structured this block"
/// copy without handling a 404.
#[tracing::instrument(skip(app), fields(session_id = %session_id, task_id = %task_id))]
pub async fn get_task_blocker(
    State(app): State<ChatAppState>,
    Path((session_id, task_id)): Path<(Uuid, String)>,
) -> impl IntoResponse {
    let Some(session) = app.conversation.get_session(session_id).await else {
        return (StatusCode::NOT_FOUND, "session not found").into_response();
    };
    let Some(pkg) = session.emitted_package_path.clone() else {
        return Json(serde_json::json!({ "blocker": null, "attempts": [] })).into_response();
    };
    // Path-jail: the permissive form that silently drops the
    // check when EITHER canonicalize call returns Err is a footgun —
    // first branch on `pkg.canonicalize()` (e.g., package dir doesn't
    // exist yet) and again on `path.canonicalize()` (e.g., blocker.json
    // absent because the agent hasn't written it yet — extremely common
    // since the endpoint is polled by the BlockerCard). safe_segment_join
    // rejects '..' / absolute / separator-bearing task_ids up front with
    // 400, and assert_under_root canonicalizes the longest existing
    // prefix instead of blindly skipping when the leaf is missing.
    //
    // BadRoot (pkg.canonicalize() fails — typically because the
    // package dir hasn't been emitted yet or was pruned between the
    // session record being written and the BlockerCard polling) must
    // preserve the pre-jail null-shape response. Returning 403 here
    // would (a) break every polling caller that expects
    // `{blocker: null, attempts: []}` for an unemitted package and
    // (b) leak the absolute server-side package path into the body.
    // Reserve 403 for genuine Escape (symlink/canonicalize escape from
    // an existing root).
    let path = match super::super::_path_jail::runtime_outputs_for_task(&pkg, &task_id) {
        Ok(p) => p.join("blocker.json"),
        Err(super::super::PathJailError::BadRoot(_)) => {
            // Package dir hasn't been emitted yet — preserve the null
            // shape so polling callers (BlockerCard) don't error.
            return Json(serde_json::json!({ "blocker": null, "attempts": [] })).into_response();
        }
        Err(e) => {
            return (StatusCode::BAD_REQUEST, format!("invalid task_id: {}", e)).into_response();
        }
    };
    match super::super::assert_under_root(&pkg, &path) {
        Ok(()) => {}
        Err(super::super::PathJailError::BadRoot(_)) => {
            return Json(serde_json::json!({ "blocker": null, "attempts": [] })).into_response();
        }
        Err(e) => {
            return (StatusCode::FORBIDDEN, format!("path escapes root: {}", e)).into_response();
        }
    }
    let blocker_json: serde_json::Value = match tokio::fs::read(&path).await {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null),
        Err(_) => serde_json::Value::Null,
    };
    // Merge in attempts from WORKFLOW.json so the BlockerCard's
    // "Tried so far" rendering doesn't need a second round-trip. The
    // agent owns blocker.json; the harness owns WORKFLOW.json — the
    // attempts log lives on the latter (task.state.record.attempts).
    let attempts = read_task_attempts(&pkg, &task_id).unwrap_or_default();
    Json(serde_json::json!({
        "blocker": blocker_json,
        "attempts": attempts,
    }))
    .into_response()
}

#[derive(Debug, Deserialize)]
pub struct SmeDecisionsRequest {
    /// Matches the file's own `task_id` field (included for
    /// auditability + to catch misrouted writes).
    pub task_id: String,
    /// One entry per decision_point the SME answered. Empty array is
    /// rejected — the UI must post at least one answer. Partial
    /// submissions are allowed (SME answers 1 of 3 decision_points on
    /// blocker.json with multi-part questions); the agent's
    /// structured-decision re-entry handles missing answers per its
    /// prompt contract.
    pub decisions: Vec<SmeDecisionAnswer>,
    /// Optional free-form SME justification surfaced across every
    /// emitted `AppliedStructuredDecision` record for this round.
    #[serde(default)]
    pub rationale: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct SmeDecisionAnswer {
    pub id: String,
    pub chosen: String,
    #[serde(default)]
    pub rationale: Option<String>,
}

impl Clone for SmeDecisionAnswer {
    fn clone(&self) -> Self {
        Self {
            id: self.id.clone(),
            chosen: self.chosen.clone(),
            rationale: self.rationale.clone(),
        }
    }
}

/// Write `runtime/outputs/<task_id>/sme-decisions.json` (the file the
/// agent's prompt contract reads on re-entry), record one
/// `AppliedStructuredDecision` entry per answer in both the
/// in-memory session decision log + the on-disk
/// `runtime/decisions.jsonl`, and fire the auto-relaunch hook so the
/// harness resumes.
pub async fn post_sme_decisions(
    State(app): State<ChatAppState>,
    Path((session_id, task_id)): Path<(Uuid, String)>,
    Json(req): Json<SmeDecisionsRequest>,
) -> impl IntoResponse {
    if req.decisions.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            "decisions array must contain at least one answer",
        )
            .into_response();
    }
    if req.task_id != task_id {
        return (
            StatusCode::BAD_REQUEST,
            format!("task_id mismatch: path={} body={}", task_id, req.task_id),
        )
            .into_response();
    }
    let Some(session) = app.conversation.get_session(session_id).await else {
        return (StatusCode::NOT_FOUND, "session not found").into_response();
    };
    let Some(pkg) = session.emitted_package_path.clone() else {
        return (StatusCode::BAD_REQUEST, "session has no emitted package").into_response();
    };

    // Write sme-decisions.json atomically (tempfile + rename) so the
    // agent never sees a partial file when it polls on re-entry. The
    // out_dir resolves through the centralized path-jail helper.
    let out_dir = match super::super::_path_jail::runtime_outputs_for_task(&pkg, &task_id) {
        Ok(p) => p,
        Err(e) => {
            return (StatusCode::BAD_REQUEST, format!("invalid task_id: {}", e)).into_response();
        }
    };
    if let Err(e) = std::fs::create_dir_all(&out_dir) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("failed to create output dir: {}", e),
        )
            .into_response();
    }
    let body = serde_json::json!({
        "task_id": task_id,
        "timestamp": ecaa_workflow_core::time_helpers::now_rfc3339(),
        "decisions": req.decisions.iter().map(|d| {
            let mut obj = serde_json::json!({
                "id": d.id,
                "chosen": d.chosen,
            });
            if let Some(r) = &d.rationale {
                if !r.trim().is_empty() {
                    obj["rationale"] = serde_json::Value::String(r.clone());
                }
            }
            obj
        }).collect::<Vec<_>>(),
        "rationale": req.rationale,
    });
    let final_path = out_dir.join("sme-decisions.json");
    // Belt-and-suspenders canonicalize check now that the parent
    // exists. Rejects symlink-based escapes that segment-only checks
    // would miss.
    if let Err(e) = super::super::assert_under_root(&pkg, &final_path) {
        return (StatusCode::FORBIDDEN, format!("path escapes root: {}", e)).into_response();
    }
    let payload = match serde_json::to_vec_pretty(&body) {
        Ok(v) => v,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("serialize failed: {}", e),
            )
                .into_response();
        }
    };
    if let Err(e) =
        ecaa_workflow_core::fs_helpers::atomic_write_bytes_sync(&final_path, &payload)
    {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("atomic write failed: {}", e),
        )
            .into_response();
    }

    // Record one AppliedStructuredDecision per answer in the
    // session's decision log. These surface in the Decisions tab +
    // flush to runtime/decisions.jsonl on next emit. Without this
    // step the SME's answer is invisible to the audit trail (the
    // curl-based rescue path had this bug).
    let session_rationale = req.rationale.clone();
    let answers = req.decisions.clone();
    let task_id_clone = task_id.clone();
    let _ = app
        .conversation
        .store_handle()
        .update(session_id, move |s| {
            for a in &answers {
                s.record_decision(
                    ecaa_workflow_core::decision_log::DecisionType::AppliedStructuredDecision {
                        task_id: task_id_clone.clone().into(),
                        decision_point_id: a.id.clone(),
                        chosen: a.chosen.clone(),
                    },
                    ecaa_workflow_core::decision_log::DecisionActor::Sme,
                    a.rationale.clone().or_else(|| session_rationale.clone()),
                );
            }
            Ok(())
        })
        .await;

    // Resume blocked tasks in WORKFLOW.json before auto-relaunch — without
    // this the task stays Blocked, the auto-relaunch predicate sees no
    // ready task, and the harness sleeps in waiting_for_sme forever.
    // Mirrors the /unblock handler's resume hook so a structured-decision
    // response unblocks the DAG the same way an unblock click does.
    if let Err(e) = super::super::execution::resume_blocked_tasks_in_workflow(&pkg).await {
        eprintln!(
            "[sme_decisions] WORKFLOW.json resume failed for {}: {}",
            pkg.display(),
            e
        );
    }

    // Auto-relaunch the harness (predicate handles the "still
    // blocked / already running / no ready task" edges).
    super::super::execution::maybe_auto_relaunch_harness(&app, session_id, "sme_decisions").await;

    Json(serde_json::json!({
        "task_id": task_id,
        "written": true,
        "decisions_count": req.decisions.len(),
    }))
    .into_response()
}

/// Read `task.state.record.attempts` for one task from WORKFLOW.json.
/// Returns an empty vec when the task isn't Blocked, when the field is
/// absent, or when WORKFLOW.json is unreadable. Each attempt is the
/// canonical `{ method: string, result: string }` shape per CLAUDE.md.
fn read_task_attempts(
    pkg: &std::path::Path,
    task_id: &str,
) -> Result<Vec<serde_json::Value>, std::io::Error> {
    let raw = std::fs::read_to_string(pkg.join("WORKFLOW.json"))?;
    let value: serde_json::Value = serde_json::from_str(&raw)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;
    let attempts = value
        .get("tasks")
        .and_then(|t| t.get(task_id))
        .and_then(|t| t.get("state"))
        .and_then(|s| s.get("record"))
        .and_then(|r| r.get("attempts"))
        .and_then(|a| a.as_array())
        .cloned()
        .unwrap_or_default();
    Ok(attempts)
}

pub(crate) fn routes() -> axum::Router<ChatAppState> {
    axum::Router::new()
        .route(
            "/api/chat/session/:id/auto-approve-discoveries",
            axum::routing::post(auto_approve_discoveries),
        )
        .route(
            "/api/chat/session/:id/task/:task_id/sme-selection",
            axum::routing::post(post_sme_selection),
        )
        .route(
            "/api/chat/session/:id/task/:task_id/blocker",
            axum::routing::get(get_task_blocker),
        )
        .route(
            "/api/chat/session/:id/task/:task_id/sme-decisions",
            axum::routing::post(post_sme_decisions),
        )
}

#[cfg(test)]
mod tests {
    use crate::chat_routes::test_support::{
        body_json, make_router, seed_session_with_completed_task,
    };
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::util::ServiceExt;
    use uuid::Uuid;

    // ── path-jail resolver regression ─────────────────────────────────────
    //
    // Asserts the `runtime_outputs_for_task` resolver itself rejects
    // traversal. Handler-level integration is covered by the existing
    // endpoint tests; this guards the centralized helper directly.
    #[test]
    fn blocker_handlers_reject_traversal_task_id() {
        let tmp = tempfile::tempdir().unwrap();
        let pkg = tmp.path();
        assert!(super::super::super::_path_jail::runtime_outputs_for_task(pkg, "../etc").is_err());
    }

    // ── compute_allow_deny: registry-derived allowlist ────────────────────
    //
    // The endpoint's pre-fix hardcoded const wrote a 5-entry "allow"
    // list with 4 entries that didn't correspond to any real atom. The
    // new path walks the on-disk atom registry — adding a new atom
    // with `candidate_tools` or `method_choice.deferred_to` extends the
    // allowlist with zero code changes here.

    /// Production path: real `config/stage-atoms/` registry yields the
    /// proteomics + 3D-chromatin + axis-route axes the prior const
    /// missed. The high-stakes deny list overrides any conflict.
    #[test]
    fn compute_allow_deny_walks_real_registry_and_excludes_high_stakes() {
        let config_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("config");
        let (allow, deny) = super::compute_allow_deny(&config_dir);
        // The proteomics axes that bit the pre-fix path.
        for required in [
            "peptide_search",
            "protein_quantification",
            "hla_peptide_search",
            "alignment",
            "peak_calling",
        ] {
            assert!(
                allow.contains(&required.to_string()),
                "allow missing {required:?}; got {} entries: {:?}",
                allow.len(),
                allow
            );
        }
        // The four method_choice axis stems must come through (atoms
        // route via path 1 → axis ≠ atom_id).
        for axis in [
            "dtu_method",
            "isoform_caller",
            "spatial_clustering_method",
            "time_series_method",
        ] {
            assert!(
                allow.contains(&axis.to_string()),
                "allow missing method_choice axis {axis:?}"
            );
        }
        // High-stakes axes must NOT appear in allow.
        for high_stakes in [
            "differential_expression",
            "batch_correction",
            "cell_type_annotation",
            "pathway_enrichment",
        ] {
            assert!(
                !allow.contains(&high_stakes.to_string()),
                "high-stakes axis {high_stakes:?} must be excluded from allow"
            );
            assert!(
                deny.contains(&high_stakes.to_string()),
                "high-stakes axis {high_stakes:?} must appear in deny (defensive double-entry)"
            );
        }
        // Sanity bound — registry has 80+ atoms, expect 40+ axes minus
        // the high-stakes count.
        assert!(
            allow.len() >= 30,
            "production allowlist has {} entries; expected 30+. Got: {:?}",
            allow.len(),
            allow
        );
    }

    /// Fallback path: missing registry dir → tiny fallback list, never
    /// an empty file (an empty allow would block everything, defeating
    /// the checkbox's purpose entirely).
    #[test]
    fn compute_allow_deny_falls_back_when_registry_missing() {
        let tmp = tempfile::tempdir().unwrap();
        // No `stage-atoms/` under this dir → registry load returns Ok(empty),
        // which surfaces as a tiny fallback after `discover_axes()` returns
        // empty. Test the truly-broken case: pass a path that doesn't exist.
        let bogus = tmp.path().join("does-not-exist");
        let (allow, deny) = super::compute_allow_deny(&bogus);
        // Empty-registry path still returns FALLBACK_AUTO_APPROVE_ALLOW
        // when the loader can't fail outright. Just assert we never
        // return an empty allow list — that would make the checkbox a
        // no-op in the degraded state too.
        assert!(
            !allow.is_empty(),
            "compute_allow_deny must never return an empty allow list \
             (even on registry load failure) — empty == block-everything, \
             defeating the checkbox"
        );
        // Deny list is the high-stakes const regardless.
        assert!(deny.contains(&"differential_expression".to_string()));
    }

    // ── get_task_blocker endpoint ─────────────────────────────────────────

    #[tokio::test]
    async fn blocker_endpoint_returns_null_when_session_has_no_package() {
        let (router, app) = make_router(vec![]).await;
        let id = seed_session_with_completed_task(&app, "t_demo", None).await;
        let req = Request::builder()
            .method("GET")
            .uri(format!("/api/chat/session/{}/task/t_demo/blocker", id))
            .body(Body::empty())
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp.into_body()).await;
        assert!(body["blocker"].is_null());
    }

    #[tokio::test]
    async fn blocker_endpoint_returns_null_when_file_absent() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (router, app) = make_router(vec![]).await;
        let id =
            seed_session_with_completed_task(&app, "t_demo", Some(tmp.path().to_path_buf())).await;
        let req = Request::builder()
            .method("GET")
            .uri(format!("/api/chat/session/{}/task/t_demo/blocker", id))
            .body(Body::empty())
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp.into_body()).await;
        assert!(body["blocker"].is_null());
    }

    /// Path-jail regression: when the session has an
    /// `emitted_package_path` whose directory is missing from disk
    /// (transient — emit didn't finish, or another process cleaned
    /// the scratch dir), the BlockerCard's poll must still see the
    /// null-shape 200 response. A `BadRoot` 403 with the server's
    /// absolute path in the body would (a) break the polling-callsite
    /// contract and (b) leak host filesystem info to the browser.
    #[tokio::test]
    async fn blocker_endpoint_returns_null_when_package_dir_missing() {
        let tmp = tempfile::TempDir::new().unwrap();
        // Point the session at a subdir that doesn't exist yet — emulates
        // the package-not-emitted / package-pruned race the BlockerCard
        // poll can hit.
        let missing_pkg = tmp.path().join("never-emitted");
        assert!(
            !missing_pkg.exists(),
            "precondition: package dir must not exist"
        );
        let (router, app) = make_router(vec![]).await;
        let id = seed_session_with_completed_task(&app, "t_demo", Some(missing_pkg)).await;
        let req = Request::builder()
            .method("GET")
            .uri(format!("/api/chat/session/{}/task/t_demo/blocker", id))
            .body(Body::empty())
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::OK,
            "missing package dir must surface as 200 null-shape, not 403"
        );
        let body = body_json(resp.into_body()).await;
        assert!(
            body["blocker"].is_null(),
            "blocker field must be null when package dir is absent"
        );
        assert!(
            body["attempts"].is_array(),
            "attempts field must be an array (possibly empty)"
        );
    }

    #[tokio::test]
    async fn blocker_endpoint_returns_parsed_blocker_json() {
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp
            .path()
            .join("runtime")
            .join("outputs")
            .join("bio_interp");
        std::fs::create_dir_all(&dir).unwrap();
        let blocker = serde_json::json!({
            "blocker_kind": "runtime_substitution",
            "decision_points_for_sme": [
                {
                    "id": "interpretation_runtime_substitution",
                    "question": "R fgsea missing. How should we proceed?",
                    "options": [
                        {"id": "authorise_gseapy", "label": "Use gseapy"},
                        {"id": "install_r_fgsea", "label": "Install R packages"},
                    ],
                    "default_if_unanswered": "authorise_gseapy",
                }
            ],
        });
        std::fs::write(
            dir.join("blocker.json"),
            serde_json::to_vec_pretty(&blocker).unwrap(),
        )
        .unwrap();

        let (router, app) = make_router(vec![]).await;
        let id =
            seed_session_with_completed_task(&app, "bio_interp", Some(tmp.path().to_path_buf()))
                .await;

        let req = Request::builder()
            .method("GET")
            .uri(format!("/api/chat/session/{}/task/bio_interp/blocker", id))
            .body(Body::empty())
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp.into_body()).await;
        assert_eq!(body["blocker"]["blocker_kind"], "runtime_substitution");
        let dps = body["blocker"]["decision_points_for_sme"]
            .as_array()
            .unwrap();
        assert_eq!(dps.len(), 1);
        assert_eq!(dps[0]["id"], "interpretation_runtime_substitution");
        assert_eq!(dps[0]["default_if_unanswered"], "authorise_gseapy");
    }

    #[tokio::test]
    async fn blocker_endpoint_404_when_session_missing() {
        let (router, _) = make_router(vec![]).await;
        let bogus = Uuid::new_v4();
        let req = Request::builder()
            .method("GET")
            .uri(format!("/api/chat/session/{}/task/whatever/blocker", bogus))
            .body(Body::empty())
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    // ── post_sme_decisions endpoint ───────────────────────────────────────

    #[tokio::test]
    async fn sme_decisions_400_when_empty_array() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (router, app) = make_router(vec![]).await;
        let id =
            seed_session_with_completed_task(&app, "bio_interp", Some(tmp.path().to_path_buf()))
                .await;
        let req = Request::builder()
            .method("POST")
            .uri(format!(
                "/api/chat/session/{}/task/bio_interp/sme-decisions",
                id
            ))
            .header("content-type", "application/json")
            .body(Body::from(r#"{"task_id":"bio_interp","decisions":[]}"#))
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn sme_decisions_400_when_path_body_task_id_mismatch() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (router, app) = make_router(vec![]).await;
        let id =
            seed_session_with_completed_task(&app, "bio_interp", Some(tmp.path().to_path_buf()))
                .await;
        let req = Request::builder()
            .method("POST")
            .uri(format!(
                "/api/chat/session/{}/task/bio_interp/sme-decisions",
                id
            ))
            .header("content-type", "application/json")
            .body(Body::from(
                r#"{"task_id":"some_other","decisions":[{"id":"x","chosen":"y"}]}"#,
            ))
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn sme_selection_writes_both_selection_and_review_confirmed_sidecar() {
        // post_sme_selection has two file-write obligations:
        //   1. runtime/outputs/<task_id>/sme-selection.json — read by the agent
        //      on resume to commit the picked method
        //   2. runtime/sme-review-confirmed-<task_id>.json — read by the
        //      harness scheduler's filter_picks_respecting_sme_gate so
        //      downstream tasks can dispatch past the review gate
        //
        // The second was missing for a while; without it the agent picked
        // the right method, the discover_* task completed, but every
        // downstream compute task stayed pinned at Ready forever because
        // has_unconfirmed_review_ancestor refused to clear them. This test
        // pins both writes.
        let tmp = tempfile::TempDir::new().unwrap();
        let (router, app) = make_router(vec![]).await;
        let id = seed_session_with_completed_task(
            &app,
            "discover_clinical_endpoint_analysis",
            Some(tmp.path().to_path_buf()),
        )
        .await;
        let req = Request::builder()
            .method("POST")
            .uri(format!(
                "/api/chat/session/{}/task/discover_clinical_endpoint_analysis/sme-selection",
                id
            ))
            .header("content-type", "application/json")
            .body(Body::from(r#"{"chosen":"r_mmrm"}"#))
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);

        let selection = tmp
            .path()
            .join("runtime/outputs/discover_clinical_endpoint_analysis/sme-selection.json");
        assert!(selection.exists(), "sme-selection.json must exist");
        let sel_json: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&selection).unwrap()).unwrap();
        assert_eq!(sel_json["chosen"], "r_mmrm");

        let sidecar = tmp
            .path()
            .join("runtime/sme-review-confirmed-discover_clinical_endpoint_analysis.json");
        assert!(
            sidecar.exists(),
            "sme-review-confirmed-<task_id>.json sidecar must exist so the \
             harness scheduler can dispatch downstream tasks past the review gate"
        );
        let side_json: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&sidecar).unwrap()).unwrap();
        assert_eq!(side_json["stage"], "discover_clinical_endpoint_analysis");
        assert_eq!(side_json["via"], "sme_selection");
        assert_eq!(side_json["chosen"], "r_mmrm");
    }

    /// /sme-selection must flip the picked task out of Blocked in
    /// WORKFLOW.json so the auto-relaunched harness can re-dispatch
    /// it. The earlier handler wrote sme-selection.json + the review
    /// sidecar but left WORKFLOW.json's `state.status: "blocked"` in
    /// place, so the SME's pick was recorded but the task never
    /// resumed without a second `/unblock` call. This test pins the
    /// transition.
    #[tokio::test]
    async fn sme_selection_resumes_blocked_task_in_workflow_json() {
        let tmp = tempfile::TempDir::new().unwrap();
        // Seed an emitted package with one blocked task in WORKFLOW.json.
        let workflow = serde_json::json!({
            "tasks": {
                "discover_normalisation": {
                    "state": { "status": "blocked" },
                },
            },
        });
        std::fs::write(
            tmp.path().join("WORKFLOW.json"),
            serde_json::to_vec_pretty(&workflow).unwrap(),
        )
        .unwrap();

        let (router, app) = make_router(vec![]).await;
        let id = seed_session_with_completed_task(
            &app,
            "discover_normalisation",
            Some(tmp.path().to_path_buf()),
        )
        .await;

        let req = Request::builder()
            .method("POST")
            .uri(format!(
                "/api/chat/session/{}/task/discover_normalisation/sme-selection",
                id
            ))
            .header("content-type", "application/json")
            .body(Body::from(r#"{"chosen":"deseq2_vst"}"#))
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);

        // WORKFLOW.json must now show the task as ready, not blocked.
        let on_disk: serde_json::Value =
            serde_json::from_slice(&std::fs::read(tmp.path().join("WORKFLOW.json")).unwrap())
                .unwrap();
        let status = on_disk["tasks"]["discover_normalisation"]["state"]["status"]
            .as_str()
            .expect("state.status must be present");
        assert_eq!(
            status, "ready",
            "/sme-selection must resume the blocked task so the harness re-dispatches; \
             got state.status={status:?}"
        );
    }

    #[tokio::test]
    async fn sme_decisions_writes_file_and_records_decision_entries() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (router, app) = make_router(vec![]).await;
        let id =
            seed_session_with_completed_task(&app, "bio_interp", Some(tmp.path().to_path_buf()))
                .await;
        let req = Request::builder()
            .method("POST")
            .uri(format!(
                "/api/chat/session/{}/task/bio_interp/sme-decisions",
                id
            ))
            .header("content-type", "application/json")
            .body(Body::from(
                r#"{
                    "task_id": "bio_interp",
                    "decisions": [
                        {"id": "interp_runtime", "chosen": "authorise_gseapy"},
                        {"id": "collection_scope", "chosen": "hallmark_plus_reactome"}
                    ],
                    "rationale": "gseapy is the same algorithm; Hallmark+Reactome match the prior choice."
                }"#,
            ))
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp.into_body()).await;
        assert_eq!(body["written"], true);
        assert_eq!(body["decisions_count"], 2);

        // Verify the file.
        let written = tmp
            .path()
            .join("runtime/outputs/bio_interp/sme-decisions.json");
        assert!(written.exists(), "sme-decisions.json must exist on disk");
        let on_disk: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&written).unwrap()).unwrap();
        assert_eq!(on_disk["task_id"], "bio_interp");
        assert_eq!(on_disk["decisions"].as_array().unwrap().len(), 2);
        assert_eq!(on_disk["decisions"][0]["id"], "interp_runtime");

        // Verify two AppliedStructuredDecision entries are present in
        // the session's in-memory decision log.
        let session = app.conversation.get_session(id).await.unwrap();
        let applied: Vec<_> =
            session
                .decisions
                .iter()
                .filter(|d| {
                    matches!(
                    d.decision,
                    ecaa_workflow_core::decision_log::DecisionType::AppliedStructuredDecision {
                        ..
                    }
                )
                })
                .collect();
        assert_eq!(applied.len(), 2);
    }

    #[tokio::test]
    async fn sme_decisions_400_when_session_has_no_package() {
        let (router, app) = make_router(vec![]).await;
        let id = seed_session_with_completed_task(&app, "bio_interp", None).await;
        let req = Request::builder()
            .method("POST")
            .uri(format!(
                "/api/chat/session/{}/task/bio_interp/sme-decisions",
                id
            ))
            .header("content-type", "application/json")
            .body(Body::from(
                r#"{"task_id":"bio_interp","decisions":[{"id":"x","chosen":"y"}]}"#,
            ))
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    // ── Path-jail ──────────────────────────────────────
    //
    // Both `post_sme_selection` and `post_sme_decisions` join a URL-path
    // `task_id` into pkg/runtime/outputs/<task_id>/. Without a
    // canonicalize check a hostile session_id holder could write JSON
    // anywhere the server has write access; both handlers route through
    // `safe_segment_join` so `..` / absolute / separator-bearing
    // task_ids are rejected with 400.

    #[tokio::test]
    async fn post_sme_selection_rejects_traversal_task_id() {
        // axum percent-decodes URL-path params before they reach our
        // handler, so the literal `..` after decoding is what we pass
        // through; encoding as %2E%2E checks the decode path too.
        let tmp = tempfile::TempDir::new().unwrap();
        let (router, app) = make_router(vec![]).await;
        let id =
            seed_session_with_completed_task(&app, "bio_interp", Some(tmp.path().to_path_buf()))
                .await;
        let req = Request::builder()
            .method("POST")
            .uri(format!(
                "/api/chat/session/{}/task/%2E%2E%2Fescape/sme-selection",
                id
            ))
            .header("content-type", "application/json")
            .body(Body::from(r#"{"chosen":"x"}"#))
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::BAD_REQUEST,
            "traversal task_id must be rejected with 400"
        );
    }

    #[tokio::test]
    async fn post_sme_decisions_rejects_traversal_task_id() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (router, app) = make_router(vec![]).await;
        let id =
            seed_session_with_completed_task(&app, "bio_interp", Some(tmp.path().to_path_buf()))
                .await;
        // Body's task_id must match the URL's; otherwise the
        // body-path-mismatch guard fires first. To exercise the jail
        // we set both to the same traversal string so the URL-decoded
        // segment is what reaches safe_segment_join.
        let req = Request::builder()
            .method("POST")
            .uri(format!(
                "/api/chat/session/{}/task/%2E%2E%2Fescape/sme-decisions",
                id
            ))
            .header("content-type", "application/json")
            .body(Body::from(
                r#"{"task_id":"../escape","decisions":[{"id":"x","chosen":"y"}]}"#,
            ))
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::BAD_REQUEST,
            "traversal task_id must be rejected with 400"
        );
    }
}
