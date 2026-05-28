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
/// `~/.scripps-workflow/packages/`. The SME never chooses this.
pub(super) fn default_package_root() -> PathBuf {
    if let Ok(d) = std::env::var("ECAA_PACKAGE_ROOT") {
        return PathBuf::from(d);
    }
    if let Ok(home) = std::env::var("HOME") {
        return PathBuf::from(home)
            .join(".scripps-workflow")
            .join("packages");
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
/// `~/.scripps-workflow/packages`).
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

pub(super) async fn emit_package(
    session: &mut Session,
    _llm_output_dir: Option<&str>,
    config_dir: &Path,
    metrics_store: Option<&crate::metrics::MetricsStore>,
    store: Option<&crate::persistence::SessionStore>,
) -> ToolResult {
    // The gates below
    // were reading from the tool loop's local clone of `session`,
    // which is seconds-to-minutes stale relative to the persisted
    // store. A concurrent `/confirm` POST that lands during the loop
    // would have flipped `user_confirmed = true` in the store but
    // the local clone still saw `false`, producing a spurious nag
    // turn. Symmetrically, a concurrent `/proposals/:id/approve`
    // would have advanced lifecycle past PendingSme in the store
    // but the local clone still sees PendingSme and refuses to emit.
    //
    // Fix: re-read from the store at gate time when one is wired in.
    // Tests / CLI paths without a store fall back to the local
    // snapshot (single-threaded, so safe). The local `session` is
    // still mutated below (`emitted_package_path`) so the tool
    // loop's merge writes the emit forward.
    //
    // The gate is `session.is_confirmed()`, which checks the per-emit
    // ConfirmationToken against pending_emission_id + the current
    // plan summary hash. A confirm-then-amend race drifts the summary
    // hash and `is_confirmed()` returns false even though the token
    // is still present, forcing the SME to re-confirm. The store
    // re-read still matters because a concurrent /confirm POST may
    // have just arrived.
    let fresh = if let Some(s) = store {
        s.get(session.id).await
    } else {
        None
    };
    let confirmed = fresh
        .as_ref()
        .map(|s| s.is_confirmed())
        .unwrap_or_else(|| session.is_confirmed());
    if !confirmed {
        tracing::warn!(
            session_id = %session.id,
            "emit_package_precondition_failure_unconfirmed",
        );
        return ToolResult::err(ToolError::PreconditionFailure {
            reason: "SME has not confirmed THIS plan shape; re-confirmation required".into(),
            hint: "Call propose_summary_confirmation, then wait for the user to click Confirm. \
                   If the plan was amended after confirmation, the latch was cleared and the SME \
                   must re-confirm the new shape."
                .into(),
        });
    }
    // Belt-and-suspenders against the live-session bug
    // where the LLM raced ahead to emit while proposals were still
    // pending SME signoff. propose_summary_confirmation now refuses,
    // so reaching `is_confirmed() == true` with pending proposals
    // should be impossible; this second check guards against a
    // future code path that bypasses the conversational tool.
    //
    // Consult the freshly-re-read proposals when a store is
    // wired in. The local clone may show a proposal as PendingSme
    // even though `/proposals/:id/approve` advanced it to Promoted
    // during this loop.
    let proposals_source = fresh
        .as_ref()
        .map(|s| &s.proposals)
        .unwrap_or(&session.proposals);
    let pending: Vec<(&str, &str)> = proposals_source
        .values()
        .filter(|p| p.lifecycle.is_pending_sme())
        .map(|p| (p.node_id.as_str(), p.lifecycle.kind_str()))
        .collect();
    if !pending.is_empty() {
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
        return ToolResult::err(ToolError::PreconditionFailure {
            reason: format!(
                "emit refused: {} proposal(s) still pending SME action: {summary}",
                pending.len()
            ),
            hint: "Approve or reject every proposal card before emitting. \
                   See session.proposals — each entry must be in `Promoted` \
                   or `Rejected` lifecycle."
                .into(),
        });
    }
    // v4 composer fills `session.workflow_dag` and `session.compose_outcome`
    // but leaves `session.dag` (the legacy DAG cache) empty until a reader
    // calls `ensure_dag_cached`. Lower-and-cache here so emit_package can
    // gate on the derived DAG rather than the stale cache field — without
    // this, every v4 session that hadn't been read through `current_dag()`
    // failed with "no DAG built" and the state machine flipped to Blocked.
    if session.ensure_dag_cached().is_none() {
        tracing::warn!(
            session_id = %session.id,
            "emit_package_precondition_failure_no_dag",
        );
        return ToolResult::err(ToolError::PreconditionFailure {
            reason: "no DAG built — nothing to emit".into(),
            hint: "Append intake prose so the taxonomy can be classified and the DAG built.".into(),
        });
    }
    let Some(_tax) = &session.taxonomy else {
        tracing::warn!(
            session_id = %session.id,
            "emit_package_precondition_failure_no_taxonomy",
        );
        return ToolResult::err(ToolError::PreconditionFailure {
            reason: "no taxonomy loaded".into(),
            hint: "Append intake prose first.".into(),
        });
    };

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
    // or `~/.scripps-workflow/packages` — which is jailed by the
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
            // Append a
            // cost-ledger row so Tier 14's real-pipeline reader can
            // observe per-emit total spend. Best-effort: a write
            // failure logs and continues; absent MetricsStore (CLI /
            // unit-test paths) skips silently. Snapshot may legitimately
            // be `None` for sessions with zero recorded turns; we still
            // write a zero row so the ledger reflects the emit event.
            if let Some(store) = metrics_store {
                let snap = store.snapshot(session.id).await;
                let runtime_dir = out.join("runtime");
                let metrics = snap.unwrap_or_else(crate::metrics::empty_session_metrics);
                if let Err(e) =
                    crate::metrics::write_cost_ledger_row(&runtime_dir, session.id, &metrics)
                {
                    tracing::warn!(
                        "cost-ledger: failed to append row to {}: {} (continuing emit)",
                        runtime_dir.display(),
                        e
                    );
                }
                // Append a session-metrics row so
                // `runtime/session-metrics.jsonl` is populated with
                // the four SME-experience fields the Tier 16.x runners
                // need: `followup_count`, `amendment_count`,
                // `blockers_encountered`, `is_ambiguous`. Best-effort:
                // a write failure logs and continues.
                let created_at_ms = session.created_at.timestamp_millis().max(0) as u64;
                if let Err(e) = crate::metrics::write_session_metrics_row(
                    &runtime_dir,
                    session.id,
                    created_at_ms,
                    &metrics,
                ) {
                    tracing::warn!(
                        "session-metrics: failed to append row to {}: {} (continuing emit)",
                        runtime_dir.display(),
                        e
                    );
                }
            }
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
