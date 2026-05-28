//! Registration via filesystem path + listing.
//!
//! Owns:
//! - `POST /api/chat/session/:id/inputs/path` (`register_input_path`)
//! - `GET /api/chat/session/:id/inputs` (`list_inputs`)
//!
//! Plus the path-validation + manifest helpers shared with `upload.rs`
//! via `super::list::*`. Visibility: cross-module helpers are
//! `pub(super)` so `upload.rs` can reach them; tests stay
//! co-located here.

use super::super::BoundedJson;
use super::ChatAppState;
use axum::extract::Path;
use axum::{extract::State, http::StatusCode, response::IntoResponse, Json};
use chrono::Utc;
use ecaa_workflow_conversation::{UserInput, UserInputFile, UserInputKind};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::{Path as StdPath, PathBuf};
use uuid::Uuid;
use walkdir::WalkDir;

/// Default allowlist when `ECAA_INPUT_ROOTS` is unset. The literal
/// `${USER}` token is substituted with the session's `owner_user` at
/// validation time so every user gets their own home-scoped sandbox
/// without per-user configuration.
pub(super) const DEFAULT_INPUT_ROOTS: &str = "/home/${USER}/data";

/// Cap on the per-file sha256 + read pass. A single input file larger
/// than this is rejected with 413 — a 100 GB tarball isn't what this
/// surface is for; SMEs with that much data should mount the dir
/// directly. Override with `ECAA_INPUT_MAX_FILE_BYTES`.
pub(super) const DEFAULT_MAX_FILE_BYTES: u64 = 50 * 1024 * 1024 * 1024; // 50 GB

/// Cap on the total directory walk size. Refuses runaway registrations
/// (e.g. an SME accidentally pointing at `/home/<user>/data` when only
/// `/home/<user>/data/2025-cohort` was meant). Override with
/// `ECAA_INPUT_MAX_TOTAL_BYTES`.
pub(super) const DEFAULT_MAX_TOTAL_BYTES: u64 = 250 * 1024 * 1024 * 1024; // 250 GB

/// Cap on the per-registration file count. Same rationale: catch the
/// "registered the wrong root" mistake before we read 200 k files.
pub(super) const DEFAULT_MAX_FILES: usize = 50_000;

#[derive(Debug, Deserialize)]
pub(crate) struct RegisterPathRequest {
    /// Absolute path on the server filesystem.
    pub path: String,
    /// Optional SME-friendly label. Falls back to the directory
    /// basename when absent or empty.
    #[serde(default)]
    pub label: Option<String>,
}

/// Resolve the input-roots allowlist.
///
/// Priority:
/// 1. `ECAA_INPUT_ROOTS` env (`:`-separated list, supports `${USER}`)
/// 2. Built-in default `/home/${USER}/data`
///
/// Roots are canonicalized once (failing roots that don't exist are
/// kept as-is so the substitution is still meaningful — the path
/// validation below will surface the missing-dir error to the SME
/// rather than silently dropping the rule).
pub(super) fn allowlisted_roots(owner_user: &str) -> Vec<PathBuf> {
    let raw = std::env::var("ECAA_INPUT_ROOTS").unwrap_or_else(|_| DEFAULT_INPUT_ROOTS.to_string());
    raw.split(':')
        .filter(|s| !s.trim().is_empty())
        .map(|s| s.replace("${USER}", owner_user))
        .map(PathBuf::from)
        .map(|p| p.canonicalize().unwrap_or(p))
        .collect()
}

pub(super) fn max_file_bytes() -> u64 {
    ecaa_workflow_core::env_helpers::env_parse(
        "ECAA_INPUT_MAX_FILE_BYTES",
        DEFAULT_MAX_FILE_BYTES,
    )
}

pub(super) fn max_total_bytes() -> u64 {
    ecaa_workflow_core::env_helpers::env_parse(
        "ECAA_INPUT_MAX_TOTAL_BYTES",
        DEFAULT_MAX_TOTAL_BYTES,
    )
}

pub(super) fn max_files() -> usize {
    ecaa_workflow_core::env_helpers::env_parse("ECAA_INPUT_MAX_FILES", DEFAULT_MAX_FILES)
}

/// Returns Ok(canonicalized_path) if the supplied path exists, is a
/// directory, and resolves inside one of the allowlisted roots.
/// Returns Err(message) on any failure — the caller surfaces the
/// message verbatim to the SME so they can fix the path.
pub(super) fn validate_input_path(raw: &str, owner_user: &str) -> Result<PathBuf, String> {
    if raw.is_empty() {
        return Err("path is required".to_string());
    }
    let candidate = PathBuf::from(raw);
    if !candidate.is_absolute() {
        return Err(format!("path must be absolute (got {raw:?})"));
    }
    let canonical = candidate
        .canonicalize()
        .map_err(|e| format!("path does not exist or is not accessible: {e}"))?;
    if !canonical.is_dir() {
        return Err(format!(
            "path must be a directory (got {})",
            canonical.display()
        ));
    }
    let roots = allowlisted_roots(owner_user);
    if roots.is_empty() {
        return Err(
            "no input roots configured; set ECAA_INPUT_ROOTS or rely on the default \
            /home/${USER}/data"
                .to_string(),
        );
    }
    let inside_allowlist = roots.iter().any(|root| canonical.starts_with(root));
    if !inside_allowlist {
        let allowed = roots
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join(", ");
        return Err(format!(
            "path {} is not inside an allowlisted input root ({allowed})",
            canonical.display()
        ));
    }
    Ok(canonical)
}

/// Walk `root` recursively, building a manifest of regular files. Skips
/// hidden files / dotfiles by default (those are usually editor cruft,
/// `.DS_Store`, etc — never the user's data).
///
/// Returns Err on quota breach so the SME sees a clear "too big" /
/// "too many files" message instead of the server timing out.
pub(super) fn build_manifest(root: &StdPath) -> Result<Vec<UserInputFile>, String> {
    let max_files_cap = max_files();
    let max_total = max_total_bytes();
    let max_file = max_file_bytes();

    let mut files = Vec::new();
    let mut total_bytes: u64 = 0;
    let mut count = 0usize;

    for entry_result in WalkDir::new(root).follow_links(false).into_iter() {
        let entry = entry_result.map_err(|e| format!("walking {}: {e}", root.display()))?;
        let path = entry.path();
        // Skip hidden files / dirs.
        if path
            .file_name()
            .and_then(|n| n.to_str())
            .map(|n| n.starts_with('.'))
            .unwrap_or(false)
            && path != root
        {
            continue;
        }
        if !entry.file_type().is_file() {
            continue;
        }
        let meta = entry
            .metadata()
            .map_err(|e| format!("stat {}: {e}", path.display()))?;
        let size = meta.len();
        if size > max_file {
            return Err(format!(
                "file {} is {size} bytes, exceeds ECAA_INPUT_MAX_FILE_BYTES={max_file}",
                path.display()
            ));
        }
        total_bytes = total_bytes.saturating_add(size);
        if total_bytes > max_total {
            return Err(format!(
                "total registration size exceeds ECAA_INPUT_MAX_TOTAL_BYTES={max_total} bytes; \
                stop at a more specific subdirectory"
            ));
        }
        count += 1;
        if count > max_files_cap {
            return Err(format!(
                "registration would include more than ECAA_INPUT_MAX_FILES={max_files_cap} files; \
                stop at a more specific subdirectory"
            ));
        }
        let relpath = path
            .strip_prefix(root)
            .map_err(|e| format!("strip_prefix {}: {e}", path.display()))?
            .to_string_lossy()
            .into_owned();
        let sha = file_sha256(path).map_err(|e| format!("hashing {}: {e}", path.display()))?;
        files.push(UserInputFile {
            relpath,
            size_bytes: size,
            sha256: sha,
        });
    }
    Ok(files)
}

pub(super) fn file_sha256(path: &StdPath) -> std::io::Result<String> {
    let mut hasher = Sha256::new();
    let mut f = std::fs::File::open(path)?;
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex::encode(hasher.finalize()))
}

/// `POST /api/chat/session/:id/inputs/path`
pub(crate) async fn register_input_path(
    State(app): State<ChatAppState>,
    Path(session_id): Path<Uuid>,
    BoundedJson(req): BoundedJson<RegisterPathRequest>,
) -> impl IntoResponse {
    let session = match app.conversation.get_session(session_id).await {
        Some(s) => s,
        None => return (StatusCode::NOT_FOUND, "session not found").into_response(),
    };
    let owner_user = session.owner_user.clone();
    let canonical = match validate_input_path(&req.path, &owner_user) {
        Ok(p) => p,
        Err(msg) => return (StatusCode::BAD_REQUEST, msg).into_response(),
    };

    // Walk + hash on a blocking pool so the async runtime stays
    // responsive. A multi-GB cohort can take seconds to hash.
    let walk_root = canonical.clone();
    let join_result: Result<Result<Vec<UserInputFile>, String>, tokio::task::JoinError> =
        tokio::task::spawn_blocking(move || build_manifest(&walk_root)).await;
    let files = match join_result {
        Ok(Ok(f)) => f,
        Ok(Err(msg)) => return (StatusCode::BAD_REQUEST, msg).into_response(),
        Err(join_err) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("manifest join error: {join_err}"),
            )
                .into_response()
        }
    };

    let label = req
        .label
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| {
            canonical
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("input")
                .to_string()
        });

    let input = UserInput {
        input_id: Uuid::new_v4().simple().to_string()[..16].to_string(),
        label,
        kind: UserInputKind::LocalPath,
        root_path: canonical.to_string_lossy().into_owned(),
        files,
        registered_at: Utc::now(),
        registered_by: owner_user.clone(),
    };

    let store = app.conversation.store_handle();
    let result = store
        .update(session_id, move |s| {
            s.inputs.push(input.clone());
            Ok(())
        })
        .await;
    match result {
        Ok(s) => {
            // Mirror the inputs list into the emitted package's
            // `runtime/inputs.json` whenever the session has already
            // emitted. The emit pipeline writes inputs.json at emit
            // time only; subsequent registrations would otherwise stay
            // in the session-store and never reach the agent that
            // reads runtime/inputs.json during data_acquisition.
            // Without this propagation, the SME's mid-execution input
            // registration silently no-ops at the agent boundary.
            // Best-effort: tracing::warn on failure so emission errors
            // surface, but never fail the HTTP request.
            if let Some(pkg) = s.emitted_package_path.clone() {
                let inputs_json = serde_json::to_vec_pretty(&s.inputs).ok();
                if let Some(bytes) = inputs_json {
                    let path = pkg.join("runtime").join("inputs.json");
                    if let Some(parent) = path.parent() {
                        let _ = tokio::fs::create_dir_all(parent).await;
                    }
                    if let Err(e) = tokio::fs::write(&path, bytes).await {
                        tracing::warn!(
                            target: "register_input_path",
                            session_id = %session_id,
                            path = %path.display(),
                            error = %e,
                            "failed to mirror inputs.json into emitted package; agent will not see the newly registered input"
                        );
                    }
                }
            }
            Json(s.inputs.clone()).into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("persisting input: {e}"),
        )
            .into_response(),
    }
}

/// `GET /api/chat/session/:id/inputs`
pub(crate) async fn list_inputs(
    State(app): State<ChatAppState>,
    Path(session_id): Path<Uuid>,
) -> impl IntoResponse {
    match app.conversation.get_session(session_id).await {
        Some(s) => Json(s.inputs).into_response(),
        None => (StatusCode::NOT_FOUND, "session not found").into_response(),
    }
}

/// Per-file route inventory + builder. Documented next to its handler
/// list and used by the compile-time consistency assert in
/// `super::mod.rs` to ensure the aggregate `super::ROUTES` doesn't
/// drift from what each submodule serves.
#[allow(dead_code)] // doc-as-contract gate; consumed by const _: () assert.
pub(super) const ROUTES: &[(&str, &str)] = &[
    ("GET", "/api/chat/session/:id/inputs"),
    ("POST", "/api/chat/session/:id/inputs/path"),
];

pub(super) fn routes() -> axum::Router<ChatAppState> {
    axum::Router::new()
        .route(
            "/api/chat/session/:id/inputs",
            axum::routing::get(list_inputs),
        )
        .route(
            "/api/chat/session/:id/inputs/path",
            axum::routing::post(register_input_path),
        )
}

#[cfg(test)]
mod tests {
    use crate::chat_routes::inputs::test_helpers::allowlisted_temp;
    use crate::chat_routes::test_support::{body_json, make_router};
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::util::ServiceExt;

    #[tokio::test]
    async fn register_path_happy_path_persists_manifest() {
        let root = allowlisted_temp();
        let (router, app) = make_router(vec![]).await;
        let (id, _) = app.conversation.start_session(false).await.unwrap();

        let body = serde_json::json!({ "path": root.display().to_string() });
        let req = Request::builder()
            .method("POST")
            .uri(format!("/api/chat/session/{}/inputs/path", id))
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let payload = body_json(resp.into_body()).await;
        let arr = payload.as_array().expect("array");
        assert_eq!(arr.len(), 1, "exactly one registration");
        let entry = &arr[0];
        assert_eq!(entry["kind"].as_str(), Some("local_path"));
        assert_eq!(
            entry["root_path"].as_str(),
            Some(root.display().to_string().as_str())
        );
        let files = entry["files"].as_array().expect("files array");
        assert_eq!(files.len(), 2, "manifest sees both files");
        // Each file has a sha256.
        for f in files {
            assert_eq!(
                f["sha256"].as_str().map(|s| s.len()),
                Some(64),
                "sha256 is 64 hex chars",
            );
        }
        // Persisted to the session.
        let session = app.conversation.get_session(id).await.unwrap();
        assert_eq!(session.inputs.len(), 1);
    }

    #[tokio::test]
    async fn register_path_outside_allowlist_is_400() {
        let _ = allowlisted_temp();
        let (router, app) = make_router(vec![]).await;
        let (id, _) = app.conversation.start_session(false).await.unwrap();

        // Path-traversal-style attempt: try to register /etc which is
        // outside the configured root.
        let req = Request::builder()
            .method("POST")
            .uri(format!("/api/chat/session/{}/inputs/path", id))
            .header("content-type", "application/json")
            .body(Body::from(r#"{"path":"/etc"}"#))
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn register_path_nonexistent_is_400() {
        let _ = allowlisted_temp();
        let (router, app) = make_router(vec![]).await;
        let (id, _) = app.conversation.start_session(false).await.unwrap();

        let req = Request::builder()
            .method("POST")
            .uri(format!("/api/chat/session/{}/inputs/path", id))
            .header("content-type", "application/json")
            .body(Body::from(
                r#"{"path":"/nonexistent/path/that/does/not/exist"}"#,
            ))
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn register_path_after_emit_mirrors_inputs_json_into_package() {
        // Regression: post-emit input registration only persisted to
        // the session store before this fix, leaving the agent (which
        // reads runtime/inputs.json from the package on disk) blind to
        // newly registered SME inputs. Now the handler writes through
        // to the emitted package whenever emitted_package_path is set.
        use std::path::PathBuf;
        let root = allowlisted_temp();
        let (router, app) = make_router(vec![]).await;
        let (id, _) = app.conversation.start_session(false).await.unwrap();

        // Simulate post-emit state by flipping emitted_package_path on
        // the session directly. The test fakes a package by pointing
        // the path at a tempdir we control.
        let pkg_dir = tempfile::tempdir().expect("pkg tempdir");
        let pkg_path: PathBuf = pkg_dir.path().to_path_buf();
        app.conversation
            .store_handle()
            .update(id, {
                let pkg_path = pkg_path.clone();
                move |s| {
                    s.emitted_package_path = Some(pkg_path.clone());
                    Ok(())
                }
            })
            .await
            .unwrap();

        // POST /inputs/path while the session is in the emitted phase.
        let body = serde_json::json!({ "path": root.display().to_string() });
        let req = Request::builder()
            .method("POST")
            .uri(format!("/api/chat/session/{}/inputs/path", id))
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        // The package's runtime/inputs.json should now contain the
        // registered manifest entry.
        let inputs_json_path = pkg_path.join("runtime").join("inputs.json");
        assert!(
            inputs_json_path.exists(),
            "runtime/inputs.json must be written into the emitted package on post-emit registration; the agent reads this file at dispatch time",
        );
        let bytes = std::fs::read(&inputs_json_path).expect("read inputs.json");
        let parsed: serde_json::Value = serde_json::from_slice(&bytes).expect("valid json");
        let arr = parsed.as_array().expect("inputs.json holds an array");
        assert_eq!(arr.len(), 1, "exactly one mirrored entry");
        assert_eq!(arr[0]["kind"].as_str(), Some("local_path"));
    }

    #[tokio::test]
    async fn list_inputs_returns_registered_set() {
        let root = allowlisted_temp();
        let (router, app) = make_router(vec![]).await;
        let (id, _) = app.conversation.start_session(false).await.unwrap();

        // Register one.
        let body = serde_json::json!({ "path": root.display().to_string() });
        let req = Request::builder()
            .method("POST")
            .uri(format!("/api/chat/session/{}/inputs/path", id))
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap();
        let _ = router.clone().oneshot(req).await.unwrap();

        // List.
        let req = Request::builder()
            .method("GET")
            .uri(format!("/api/chat/session/{}/inputs", id))
            .body(Body::empty())
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let arr = body_json(resp.into_body()).await;
        assert_eq!(arr.as_array().unwrap().len(), 1);
    }
}
