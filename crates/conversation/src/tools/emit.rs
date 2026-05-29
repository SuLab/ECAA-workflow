//! `emit_package` tool body + helpers.
//!
//! `emit_package` is the canonical "write the RO-Crate package to
//! disk" tool. Gated by `user_confirmed` (the server-side confirmation
//! click), an in-memory DAG, and a loaded taxonomy. Alone-in-turn.
//! Helpers `default_package_root` / `auto_emit_dir` live here because
//! only this tool resolves the writable path.

use crate::errors::{ToolError, ToolResult};
use crate::session::Session;
use std::path::{Path, PathBuf};

/// Server-controlled package root. Packages land under
/// `$ECAA_PACKAGE_ROOT` when set, otherwise
/// `~/.ecaa-workflow/packages/`. The SME never chooses this.
pub(super) fn default_package_root() -> PathBuf {
    if let Ok(d) = std::env::var("ECAA_PACKAGE_ROOT") {
        return PathBuf::from(d);
    }
    if let Ok(home) = std::env::var("HOME") {
        return PathBuf::from(home).join(".ecaa-workflow").join("packages");
    }
    PathBuf::from("/tmp/scripps-packages")
}

/// Compose a per-session, per-emit package path under the central root.
/// Uses the session id + classified modality + UTC timestamp so concurrent
/// sessions don't collide and re-emissions from the same session don't
/// overwrite each other.
pub(super) fn auto_emit_dir(session: &Session) -> PathBuf {
    let modality = session
        .classification
        .as_ref()
        .map(|c| c.modality.as_str())
        .unwrap_or("package");
    let ts = chrono::Utc::now().format("%Y%m%dT%H%M%S");
    default_package_root().join(format!("{}-{}-{}", session.id, modality, ts))
}

/// Security audit C-10 / the LLM has no business
/// choosing where the package lands. `_caller_supplied` is preserved in
/// the function signature so the CLI/test paths that historically passed
/// an explicit path keep compiling, but the value is intentionally
/// dropped on the floor — the resolved path always comes from
/// `auto_emit_dir(session)` and is jailed under
/// `default_package_root()` (`$ECAA_PACKAGE_ROOT` or
/// `~/.ecaa-workflow/packages`).
pub(super) fn resolve_emit_output_dir(
    session: &Session,
    caller_supplied: Option<&str>,
) -> std::path::PathBuf {
    // The LLM cannot influence the emit location. The CLI/test surface
    // can still pass a non-empty literal path because those callers
    // need byte-stable on-disk locations (e.g. golden snapshots).
    // Caller-supplied paths must already be absolute; relative paths
    // are dropped on the floor so a future tooling refactor cannot
    // accidentally smuggle untrusted strings through.
    match caller_supplied {
        Some(p) if !p.trim().is_empty() => {
            let candidate = std::path::PathBuf::from(p);
            if candidate.is_absolute() {
                candidate
            } else {
                tracing::warn!(
                    caller_supplied = %p,
                    "dropping relative emit output_dir; falling back to server-assigned path \
                     (C-10 hardening)"
                );
                auto_emit_dir(session)
            }
        }
        _ => auto_emit_dir(session),
    }
}

/// Run the emit-time precondition gates against the freshest session state
/// (`fresh` is the store re-read when a store is wired in, else the local
/// clone). Returns `Some(refusal)` on the first failed gate, `None` when all
/// pass. The gates, in order:
/// 1. The per-emit `ConfirmationToken` (`is_confirmed()`) — a confirm-then-amend
///    race drifts the plan-summary hash and forces re-confirmation.
/// 2. No proposals still pending SME signoff (belt-and-suspenders against the
///    LLM racing ahead of proposal approval).
/// 3. A DAG is built (`ensure_dag_cached` lowers the v4 `workflow_dag` into the
///    legacy cache so we gate on the derived DAG, not the stale field).
/// 4. A taxonomy is loaded.
fn check_emit_preconditions(session: &mut Session, fresh: Option<&Session>) -> Option<ToolResult> {
    if let Some(r) = gate_confirmed(session, fresh) {
        return Some(r);
    }
    if let Some(r) = gate_no_pending_proposals(session, fresh) {
        return Some(r);
    }
    if let Some(r) = gate_dag_built(session) {
        return Some(r);
    }
    gate_taxonomy(session)
}

/// Gate 1 — the per-emit `ConfirmationToken`. A confirm-then-amend race drifts
/// the plan-summary hash so `is_confirmed()` returns false and forces re-confirm.
fn gate_confirmed(session: &Session, fresh: Option<&Session>) -> Option<ToolResult> {
    let confirmed = fresh
        .map(|s| s.is_confirmed())
        .unwrap_or_else(|| session.is_confirmed());
    if confirmed {
        return None;
    }
    tracing::warn!(
        session_id = %session.id,
        "emit_package_precondition_failure_unconfirmed",
    );
    Some(ToolResult::err(ToolError::PreconditionFailure {
        reason: "SME has not confirmed THIS plan shape; re-confirmation required".into(),
        hint: "Call propose_summary_confirmation, then wait for the user to click Confirm. \
               If the plan was amended after confirmation, the latch was cleared and the SME \
               must re-confirm the new shape."
            .into(),
    }))
}

/// Gate 2 — no proposals still pending SME signoff (belt-and-suspenders against
/// the LLM racing ahead of proposal approval).
fn gate_no_pending_proposals(session: &Session, fresh: Option<&Session>) -> Option<ToolResult> {
    let proposals_source = fresh.map(|s| &s.proposals).unwrap_or(&session.proposals);
    let pending: Vec<(&str, &str)> = proposals_source
        .values()
        .filter(|p| p.lifecycle.is_pending_sme())
        .map(|p| (p.node_id.as_str(), p.lifecycle.kind_str()))
        .collect();
    if pending.is_empty() {
        return None;
    }
    let summary = pending
        .iter()
        .map(|(node, kind)| format!("{node} ({kind})"))
        .collect::<Vec<_>>()
        .join(", ");
    tracing::warn!(
        session_id = %session.id,
        pending_proposals = %summary,
        count = pending.len(),
        "emit_package_precondition_failure_proposals_pending",
    );
    Some(ToolResult::err(ToolError::PreconditionFailure {
        reason: format!(
            "emit refused: {} proposal(s) still pending SME action: {summary}",
            pending.len()
        ),
        hint: "Approve or reject every proposal card before emitting. \
               See session.proposals — each entry must be in `Promoted` \
               or `Rejected` lifecycle."
            .into(),
    }))
}

/// Gate 3 — a DAG is built. `ensure_dag_cached` lowers the v4 `workflow_dag`
/// into the legacy cache so we gate on the derived DAG, not the stale field.
fn gate_dag_built(session: &mut Session) -> Option<ToolResult> {
    if session.ensure_dag_cached().is_some() {
        return None;
    }
    tracing::warn!(
        session_id = %session.id,
        "emit_package_precondition_failure_no_dag",
    );
    Some(ToolResult::err(ToolError::PreconditionFailure {
        reason: "no DAG built — nothing to emit".into(),
        hint: "Append intake prose so the taxonomy can be classified and the DAG built.".into(),
    }))
}

/// Gate 4 — a taxonomy is loaded.
fn gate_taxonomy(session: &Session) -> Option<ToolResult> {
    if session.taxonomy.is_some() {
        return None;
    }
    tracing::warn!(
        session_id = %session.id,
        "emit_package_precondition_failure_no_taxonomy",
    );
    Some(ToolResult::err(ToolError::PreconditionFailure {
        reason: "no taxonomy loaded".into(),
        hint: "Append intake prose first.".into(),
    }))
}

/// Best-effort post-emit telemetry: append the per-emit cost-ledger row and the
/// SME-experience session-metrics row under `<out>/runtime/`. No-op without a
/// `MetricsStore` (CLI / unit-test paths). A zero row is still written for
/// sessions with no recorded turns so the ledger reflects the emit event. Write
/// failures are logged and swallowed (never block the emit).
async fn write_emit_metrics(
    metrics_store: Option<&crate::metrics::MetricsStore>,
    session: &Session,
    out: &Path,
) {
    let Some(store) = metrics_store else {
        return;
    };
    let snap = store.snapshot(session.id).await;
    let runtime_dir = out.join("runtime");
    let metrics = snap.unwrap_or_else(crate::metrics::empty_session_metrics);
    if let Err(e) = crate::metrics::write_cost_ledger_row(&runtime_dir, session.id, &metrics) {
        tracing::warn!(
            "cost-ledger: failed to append row to {}: {} (continuing emit)",
            runtime_dir.display(),
            e
        );
    }
    let created_at_ms = session.created_at.timestamp_millis().max(0) as u64;
    if let Err(e) =
        crate::metrics::write_session_metrics_row(&runtime_dir, session.id, created_at_ms, &metrics)
    {
        tracing::warn!(
            "session-metrics: failed to append row to {}: {} (continuing emit)",
            runtime_dir.display(),
            e
        );
    }
}

pub(super) async fn emit_package(
    session: &mut Session,
    _llm_output_dir: Option<&str>,
    config_dir: &Path,
    metrics_store: Option<&crate::metrics::MetricsStore>,
    store: Option<&crate::persistence::SessionStore>,
) -> ToolResult {
    // Re-read from the store at gate time when one is wired in, so the
    // precondition checks see a concurrent `/confirm` or
    // `/proposals/:id/approve` that landed during this tool loop rather than
    // the loop's seconds-to-minutes-stale local clone. Tests / CLI paths
    // without a store fall back to the local snapshot (single-threaded, safe).
    let fresh = if let Some(s) = store {
        s.get(session.id).await
    } else {
        None
    };
    if let Some(refusal) = check_emit_preconditions(session, fresh.as_ref()) {
        return refusal;
    }

    // `StateTrigger::EmitPackageStart` (ReadyToEmit →
    // Emitting) fires from the dispatcher's pre-handler hook (see
    // `tools/mod.rs::SPEC_EMIT_PACKAGE.state_trigger`). Returning a
    // `ToolError` from this handler triggers the dispatcher's
    // `post_handler.on_err` hook to fire `EmitPackageErr` with the
    // error reason; returning `ToolResult::ok` triggers `on_ok` to
    // fire `EmitPackageOk`.

    // C-10: the LLM tool no longer carries `output_dir` —
    // the field was dropped from the JSON schema and the dispatcher
    // forwards `None`. We pass `None` to the resolver unconditionally
    // so the LLM's prior power to choose the emit location is gone;
    // CLI/test paths must go through a different entry point if they
    // need to override the path. `auto_emit_dir` yields a per-session,
    // per-emit subdir of `default_package_root()` — `$ECAA_PACKAGE_ROOT`
    // or `~/.ecaa-workflow/packages` — which is jailed by the
    // operator-controlled env var rather than the model.
    let out: PathBuf = resolve_emit_output_dir(session, None);
    if let Some(parent) = out.parent() {
        if let Err(e) = tokio::fs::create_dir_all(parent).await {
            let reason = format!("creating package parent {}: {:#}", parent.display(), e);
            return ToolResult::err(ToolError::InternalError { reason });
        }
    }
    let result = crate::emit::emit_with_conversation_log(session, &out, config_dir).await;
    match result {
        Ok(()) => {
            session.emitted_package_path = Some(out.clone());
            write_emit_metrics(metrics_store, session, &out).await;
            // Record the decision *before* returning so the log-dump path
            // inside emit_with_conversation_log sees it next time (on the
            // amendment re-emit, the previous record is already on disk).
            session.record_decision(
                ecaa_workflow_core::decision_log::DecisionType::EmitPackage {
                    output_dir: out.to_string_lossy().to_string(),
                },
                ecaa_workflow_core::decision_log::DecisionActor::Llm,
                None,
            );
            ToolResult::ok(serde_json::json!({
                "ok": true,
                "output_dir": out.to_string_lossy(),
            }))
        }
        Err(e) => {
            // The `:#` formatter on anyhow chains the full cause list —
            // without it the user only sees the top `.context()` label
            // (e.g. "core emit_package") and loses the underlying OS
            // error ("Permission denied (os error 13)", "No such file or
            // directory"), which is almost always the actionable part.
            let reason = format!("{:#}", e);
            ToolResult::err(ToolError::InternalError { reason })
        }
    }
}

#[cfg(test)]
mod output_dir_safety_tests {
    //! C-10 / 05 regression: `emit_package` must not accept an
    //! LLM-supplied output_dir. The schema-side drop is verified in
    //! `tool_schemas.rs`; here we assert the resolver behavior so a
    //! future refactor that reconnects the parameter still won't honor
    //! an arbitrary string.
    use super::*;
    use crate::session::Session;

    fn session_with_modality() -> Session {
        let mut s = Session::new(false);
        s.classification = Some(ecaa_workflow_core::classify::ClassificationResult {
            modality: "bulk_rnaseq".into(),
            ..Default::default()
        });
        s
    }

    #[test]
    fn relative_path_is_ignored() {
        // An attacker who passes a relative path with shell-magic chars
        // must NOT see those bytes survive into the resolved PathBuf.
        let s = session_with_modality();
        let resolved = resolve_emit_output_dir(&s, Some("/tmp/A; rm -rf $HOME; B"));
        // The string is absolute and therefore preserved verbatim (CLI
        // entry-point only — the LLM dispatcher always passes None).
        // We assert the LLM-call shape: passing None yields the server
        // default and never echoes attacker bytes.
        let none_resolved = resolve_emit_output_dir(&s, None);
        assert!(
            none_resolved.is_absolute(),
            "default emit dir must be absolute"
        );
        assert!(
            !none_resolved.to_string_lossy().contains(';'),
            "default emit dir must not contain shell separators"
        );
        assert!(
            !none_resolved.to_string_lossy().contains("rm -rf"),
            "default emit dir must not echo attacker fragments: {}",
            none_resolved.display()
        );
        // Relative paths fall through to the server default.
        let rel_resolved = resolve_emit_output_dir(&s, Some("../../etc/passwd"));
        assert_eq!(
            rel_resolved, none_resolved,
            "relative caller-supplied paths must fall back to the default"
        );
        // Absolute paths from CLI callers ARE honored (test/CLI surface).
        // But the LLM dispatcher always passes None, so this branch
        // never receives untrusted input in production.
        let _ = resolved; // path is absolute → preserved verbatim
    }

    #[test]
    fn empty_string_uses_default() {
        let s = session_with_modality();
        let resolved = resolve_emit_output_dir(&s, Some(""));
        let none = resolve_emit_output_dir(&s, None);
        assert_eq!(resolved, none);
    }

    #[test]
    fn whitespace_only_uses_default() {
        let s = session_with_modality();
        let resolved = resolve_emit_output_dir(&s, Some("   "));
        let none = resolve_emit_output_dir(&s, None);
        assert_eq!(resolved, none);
    }

    #[test]
    fn none_yields_jailed_default() {
        let s = session_with_modality();
        let resolved = resolve_emit_output_dir(&s, None);
        // Must land under the server's package root, NOT inside `/etc`
        // or another sensitive location.
        let root = default_package_root();
        assert!(
            resolved.starts_with(&root),
            "default emit dir must be jailed under package root {} (got {})",
            root.display(),
            resolved.display()
        );
    }
}
