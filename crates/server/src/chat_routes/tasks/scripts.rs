//! Per-task script surface: list `runtime/outputs/<task_id>/scripts/*`
//! for the per-task drawer's Scripts tab, and re-run a wrapper script
//! detached so the SME can re-trigger an idempotent recovery without
//! routing through the harness.
//!
//! Endpoints (plan §S16.2):
//! - GET /session/:id/task/:task_id/scripts
//! - POST /session/:id/task/:task_id/rerun-script
//!
//! Shares the recursive-walk helper with `logs.rs` via
//! `super::logs::list_task_files`.

use super::super::*;
use super::logs::{list_task_files, FileFilter};
use axum::{extract::State, http::StatusCode, response::IntoResponse, Json};
use serde::Deserialize;
use uuid::Uuid;

/// List `runtime/outputs/<task_id>/scripts/*.{R,py,sh,Rmd}` so the SME
/// can review the agent-generated code paths. Recursion depth bound is
/// the same 2 levels as `list_task_logs`.
pub async fn list_task_scripts(
    State(app): State<ChatAppState>,
    Path((session_id, task_id)): Path<(Uuid, String)>,
) -> impl IntoResponse {
    list_task_files(app, session_id, &task_id, FileFilter::Scripts).await
}

#[derive(Debug, Clone, Deserialize)]
pub struct RerunScriptRequest {
    pub rel_path: String,
}

/// Resolve a rerun target under `<pkg>/runtime/outputs/<task_id>/scripts/<rel_path>`,
/// jailing both `task_id` and `rel_path` against traversal. Returns the joined
/// path BEFORE filesystem canonicalization so the handler can report the
/// path it tried; the handler should canonicalize-verify the final target
/// stays under `pkg` after `scripts_dir.canonicalize()`.
fn resolve_rerun_target(
    pkg: &std::path::Path,
    task_id: &str,
    rel_path: &str,
) -> Result<std::path::PathBuf, StatusCode> {
    let task_dir = super::super::_path_jail::runtime_outputs_for_task(pkg, task_id)
        .map_err(|_| StatusCode::BAD_REQUEST)?;
    let scripts_dir = task_dir.join("scripts");
    let target = super::super::_path_jail::safe_segment_join(&scripts_dir, rel_path)
        .map_err(|_| StatusCode::BAD_REQUEST)?;
    super::super::_path_jail::assert_under_root(pkg, &target)
        .map_err(|_| StatusCode::BAD_REQUEST)?;
    Ok(target)
}

/// Re-run a wrapper script under `runtime/outputs/<task_id>/scripts/`.
/// The path must live inside the task's `scripts/` subdir (no escape)
/// and must end in `.sh`. Spawns a detached subprocess and returns
/// its pid; the script writes its own status sentinel which the
/// existing status-sentinels endpoint surfaces.
pub async fn post_rerun_script(
    State(app): State<ChatAppState>,
    Path((session_id, task_id)): Path<(Uuid, String)>,
    Json(req): Json<RerunScriptRequest>,
) -> impl IntoResponse {
    let Some(session) = app.conversation.get_session(session_id).await else {
        return (StatusCode::NOT_FOUND, "session not found").into_response();
    };
    let Some(pkg) = session.emitted_package_path.clone() else {
        return (StatusCode::BAD_REQUEST, "session has no emitted package").into_response();
    };
    let target = match resolve_rerun_target(&pkg, &task_id, &req.rel_path) {
        Ok(p) => p,
        Err(status) => return (status, "invalid task_id or rel_path").into_response(),
    };
    let Ok(target_canon) = target.canonicalize() else {
        return (StatusCode::NOT_FOUND, "script not found").into_response();
    };
    // Belt-and-suspenders: re-verify the canonicalized target sits under pkg
    // (the path-jail also did this; a symlink planted after the jail call
    // would be caught here, but TOCTOU on filesystem is bounded).
    if super::super::_path_jail::assert_under_root(&pkg, &target_canon).is_err() {
        return (StatusCode::FORBIDDEN, "path escapes package root").into_response();
    }
    let suffix_ok = target_canon
        .extension()
        .and_then(|s| s.to_str())
        .is_some_and(|s| s == "sh");
    if !suffix_ok {
        return (
            StatusCode::BAD_REQUEST,
            "only .sh wrappers may be rerun via this endpoint",
        )
            .into_response();
    }
    // Spawn detached. The wrapper itself manages its own output
    // redirection and writes a status sentinel — we don't supervise
    // the process beyond returning its pid.
    //
    // The pre-fix code dropped the `Child` without
    // ever calling `.wait()`. Tokio's `Child` does not implicitly reap
    // on `Drop` unless `kill_on_drop` is set; the kernel kept the
    // wrapper's exit status alive in the process table as a zombie,
    // and each rerun click accumulated one more zombie until the
    // server restarted. The fix:
    // 1. Disable `kill_on_drop` (default-off; we want the wrapper to
    // outlive the request handler, that's the entire point of the
    // endpoint — re-trigger an idempotent recovery without the
    // harness).
    // 2. Hand the `Child` to a `tokio::spawn`'d reaper task that
    // awaits `.wait()`, logs the exit status, and drops the
    // `Child` only after the kernel has acknowledged the death.
    // 3. Detach into its own process group via `setsid()` in
    // `pre_exec` so a server SIGTERM/SIGINT during a long-running
    // recovery wrapper doesn't take it down (the SME explicitly
    // asked for a fire-and-forget rerun).
    let mut cmd = tokio::process::Command::new("bash");
    cmd.arg(target_canon.as_os_str())
        .current_dir(&pkg)
        .env("PACKAGE", pkg.to_string_lossy().to_string())
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(unix)]
    {
        // tokio::process::Command's `pre_exec` is inherent on Linux
        // (mirrors `std::os::unix::process::CommandExt::pre_exec`).
        // Mirrors the import-shape used by
        // `chat_routes::execution::start::spawn_harness`; the
        // `#[allow(unused_imports)]` waiver matches that path too —
        // the unsafe block obscures the trait method use from the
        // unused-import analyzer when tokio routes it through its own
        // inherent impl.
        #[allow(unused_imports)]
        use std::os::unix::process::CommandExt;
        // SAFETY: `pre_exec` runs in the child between fork and exec,
        // when async-signal-safe rules apply. The closure body is a
        // single `setsid` syscall and `Error::last_os_error` lookup —
        // both async-signal-safe. Workspace lint is `unsafe_code =
        // "deny"` (§S5.32); this is the bounded waiver.
        #[allow(unsafe_code)]
        unsafe {
            cmd.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }
    match cmd.spawn() {
        Ok(mut child) => {
            let pid = child.id().map(|p| p as i64);
            let rel_for_log = req.rel_path.clone();
            // Reaper: own the Child until it exits so the kernel can
            // drop its process-table entry. Logs the exit status so a
            // wrapper that crashed on launch shows up in the server
            // log alongside the harness's own progress lines.
            tokio::spawn(async move {
                match child.wait().await {
                    Ok(status) => {
                        tracing::info!(
                            script = %rel_for_log,
                            pid = ?pid,
                            ?status,
                            "rerun-script wrapper exited"
                        );
                    }
                    Err(e) => {
                        tracing::warn!(
                            script = %rel_for_log,
                            pid = ?pid,
                            error = %e,
                            "rerun-script wrapper wait() failed"
                        );
                    }
                }
            });
            Json(serde_json::json!({
                "pid": pid,
                "message": format!("launched {}", req.rel_path),
            }))
            .into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("spawn failed: {}", e),
        )
            .into_response(),
    }
}

pub(crate) fn routes() -> axum::Router<ChatAppState> {
    axum::Router::new()
        .route(
            "/api/chat/session/:id/task/:task_id/scripts",
            axum::routing::get(list_task_scripts),
        )
        .route(
            "/api/chat/session/:id/task/:task_id/rerun-script",
            axum::routing::post(post_rerun_script),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rerun_script_rejects_traversal_task_id() {
        let tmp = tempfile::tempdir().unwrap();
        let pkg = tmp.path().to_path_buf();
        std::fs::create_dir_all(pkg.join("runtime/outputs/legit/scripts")).unwrap();
        std::fs::write(
            pkg.join("runtime/outputs/legit/scripts/x.sh"),
            "#!/bin/sh\n",
        )
        .unwrap();
        let result = resolve_rerun_target(&pkg, "../../etc", "x.sh");
        assert!(matches!(result, Err(StatusCode::BAD_REQUEST)));
    }

    #[test]
    fn rerun_script_rejects_traversal_rel_path() {
        let tmp = tempfile::tempdir().unwrap();
        let pkg = tmp.path().to_path_buf();
        std::fs::create_dir_all(pkg.join("runtime/outputs/legit/scripts")).unwrap();
        let result = resolve_rerun_target(&pkg, "legit", "../../etc/passwd");
        assert!(matches!(result, Err(StatusCode::BAD_REQUEST)));
    }

    #[test]
    fn rerun_script_resolves_legit_path() {
        let tmp = tempfile::tempdir().unwrap();
        let pkg = tmp.path().to_path_buf();
        std::fs::create_dir_all(pkg.join("runtime/outputs/legit/scripts")).unwrap();
        std::fs::write(
            pkg.join("runtime/outputs/legit/scripts/x.sh"),
            "#!/bin/sh\n",
        )
        .unwrap();
        let result = resolve_rerun_target(&pkg, "legit", "x.sh").unwrap();
        assert_eq!(result, pkg.join("runtime/outputs/legit/scripts/x.sh"));
    }
}
