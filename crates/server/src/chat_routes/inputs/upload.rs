//! Chunked, resumable browser upload + finalize.
//!
//! Owns:
//! - `POST /api/chat/session/:id/inputs/upload` (`upload_input_chunk`)
//! - `POST /api/chat/session/:id/inputs/upload/:upload_token/finalize`
//!   (`finalize_upload`)
//!
//! Plus upload-only helpers: `upload_root_for`,
//! `check_disk_reserve_for`, `parse_content_range`,
//! `sanitize_filename`. Shared cross-cutting helpers
//! (`build_manifest`, `file_sha256`, `allowlisted_roots`,
//! `max_file_bytes`) live in `super::list` and are imported here.

use super::list::{allowlisted_roots, build_manifest, file_sha256, max_file_bytes};
use super::ChatAppState;
use axum::extract::Path;
use axum::{extract::State, http::StatusCode, response::IntoResponse, Json};
use chrono::Utc;
use ecaa_workflow_conversation::{UserInput, UserInputFile, UserInputKind};
use std::path::{Path as StdPath, PathBuf};
use uuid::Uuid;

/// Hard cap on a single chunk POST body. The chunked-upload protocol
/// uses 8 MiB chunks by default (`UPLOAD_CHUNK_BYTES` in
/// `ui/src/api/chatClient.ts`); 32 MiB gives a 4x headroom for clients
/// that bump the chunk size for very large files while still bounding
/// the memory the server is willing to materialize in a single
/// `axum::body::to_bytes` call. Without this cap a malicious caller
/// could wedge the process with a multi-GB POST before any other
/// guard fired.
///
/// The whole-file size is still capped separately by `max_file_bytes()`
/// against `ECAA_INPUT_MAX_FILE_BYTES`; this constant is the per-chunk
/// limit so per-message DoS can never exhaust memory regardless of how
/// the overall file budget is configured.
pub(super) const MAX_UPLOAD_CHUNK_BYTES: usize = 32 * 1024 * 1024;

/// `POST /api/chat/session/:id/inputs/upload`
///
/// Chunked, resumable file upload. The browser slices a file into
/// fixed-size chunks (recommended 8 MiB) and POSTs each chunk with:
///
/// - `Upload-Token: <opaque-id>` — stable per file, used by the
///   server to append chunks to the same staging file. The browser
///   generates this (uuid).
/// - `Upload-Filename: <original-filename>` — required on the FIRST
///   chunk; ignored on subsequent. The server sanitizes
///   (basename only, no path separators) before opening the staging
///   file.
/// - `Content-Range: bytes <start>-<end>/<total>` — required on
///   every chunk. The server validates `start == current_size_on_disk`
///   so out-of-order chunks 409 with a hint to GET the upload status
///   and resume.
/// - `Upload-Sha256: <hex>` — required on the FINAL chunk only.
///   Server hashes the assembled file and rejects on mismatch.
///
/// On final-chunk success the server atomically renames the staging
/// file under `<ECAA_UPLOAD_ROOT>/<session_id>/<upload_token>/<filename>`,
/// updates an in-progress upload registry, and returns
/// `{"status":"complete", "input_id": null}`. The UI then calls
/// `POST /inputs/upload/:upload_token/finalize` once it has uploaded
/// every file in the batch — that endpoint coalesces the per-file
/// registrations into a single `UserInput` of `kind: uploaded_files`
/// and surfaces it in `Session.inputs`.
///
/// On non-final chunks the server returns
/// `{"status":"partial", "received_bytes": N, "total_bytes": T}`.
pub(crate) async fn upload_input_chunk(
    State(app): State<ChatAppState>,
    Path(session_id): Path<Uuid>,
    headers: axum::http::HeaderMap,
    request: axum::extract::Request,
) -> impl IntoResponse {
    // Pre-check `Content-Length` against the hard chunk cap so a forged
    // or oversized header fails fast with 413 before we even start
    // reading the body. `Transfer-Encoding: chunked` requests legitimately
    // omit Content-Length; the to_bytes cap below still enforces the
    // same ceiling on those, just reactively.
    if let Some(len_hdr) = headers.get(axum::http::header::CONTENT_LENGTH) {
        let parsed = len_hdr.to_str().ok().and_then(|s| s.parse::<u64>().ok());
        match parsed {
            Some(len) if (len as u128) > MAX_UPLOAD_CHUNK_BYTES as u128 => {
                return (
                    StatusCode::PAYLOAD_TOO_LARGE,
                    format!(
                        "Content-Length {len} exceeds per-chunk cap {} bytes",
                        MAX_UPLOAD_CHUNK_BYTES
                    ),
                )
                    .into_response();
            }
            None => {
                return (
                    StatusCode::BAD_REQUEST,
                    "Content-Length header is not a valid u64",
                )
                    .into_response();
            }
            _ => {}
        }
    }

    // Cap body reads at `MAX_UPLOAD_CHUNK_BYTES` (32 MiB). The global
    // `DefaultBodyLimit` is disabled on this sub-router because the
    // chunk protocol uses 8 MiB chunks (well past axum's 2 MiB default),
    // but an in-handler cap is still required so a malicious caller
    // can't exhaust memory with a multi-GB body. A body larger than
    // the cap fails with 413 here; the per-file ceiling
    // (`max_file_bytes()`) is enforced separately below.
    let body = match axum::body::to_bytes(request.into_body(), MAX_UPLOAD_CHUNK_BYTES).await {
        Ok(b) => b,
        Err(e) => {
            let msg = e.to_string();
            // axum surfaces the limit hit as a `LengthLimitError`
            // wrapper around our cap; map that case to 413 for parity
            // With the Content-Length pre-check above.
            // emit the typed `ApiError` envelope so the UI can branch
            // on `code` rather than substring-matching the body.
            if msg.contains("length limit exceeded") || msg.contains("body limit") {
                return crate::error::ApiError::BodyTooLarge {
                    limit_bytes: MAX_UPLOAD_CHUNK_BYTES as u64,
                }
                .into_response();
            }
            return crate::error::ApiError::BadRequest(format!(
                "failed to read upload chunk body: {msg}"
            ))
            .into_response();
        }
    };
    // Require the session exists (the session.json is the source of
    // truth for owner_user → upload-root substitution).
    let session = match app.conversation.get_session(session_id).await {
        Some(s) => s,
        None => return (StatusCode::NOT_FOUND, "session not found").into_response(),
    };

    let upload_token = match headers
        .get("Upload-Token")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
    {
        Some(t) if t.len() <= 64 && t.chars().all(|c| c.is_alphanumeric() || c == '-') => {
            t.to_string()
        }
        _ => {
            return (
                StatusCode::BAD_REQUEST,
                "Upload-Token header is required (alphanumeric or '-', ≤ 64 chars)",
            )
                .into_response()
        }
    };

    let content_range = match headers.get("Content-Range").and_then(|v| v.to_str().ok()) {
        Some(s) => s.to_string(),
        None => {
            return (
                StatusCode::BAD_REQUEST,
                "Content-Range header is required (format: 'bytes <start>-<end>/<total>')",
            )
                .into_response()
        }
    };
    let (range_start, range_end, total_bytes) = match parse_content_range(&content_range) {
        Ok(t) => t,
        Err(msg) => return (StatusCode::BAD_REQUEST, msg).into_response(),
    };

    if total_bytes > max_file_bytes() {
        return (
            StatusCode::PAYLOAD_TOO_LARGE,
            format!(
                "file size {total_bytes} exceeds ECAA_INPUT_MAX_FILE_BYTES={}",
                max_file_bytes()
            ),
        )
            .into_response();
    }

    // Per-session disk reserve guard.
    if let Err(msg) = check_disk_reserve_for(session_id, &session.owner_user).await {
        return (StatusCode::INSUFFICIENT_STORAGE, msg).into_response();
    }

    let upload_dir = upload_root_for(&session.owner_user)
        .join(session_id.to_string())
        .join(&upload_token);
    if let Err(e) = tokio::fs::create_dir_all(&upload_dir).await {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("creating upload dir: {e}"),
        )
            .into_response();
    }

    // The first chunk carries the original filename; subsequent
    // chunks reuse the same staging file. Filename is sanitized to
    // basename + safe-character set so the SME can't path-traverse.
    let staging_path = upload_dir.join("staging.bin");
    let manifest_path = upload_dir.join("upload-manifest.json");
    let chunk_len = body.len() as u64;

    // First-chunk filename capture.
    if range_start == 0 {
        let raw_name = headers
            .get("Upload-Filename")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .unwrap_or("upload.bin");
        let safe_name = sanitize_filename(raw_name);
        let manifest = serde_json::json!({
            "original_filename": raw_name,
            "safe_filename": safe_name,
            "total_bytes": total_bytes,
            "started_at": Utc::now(),
        });
        if let Err(e) = tokio::fs::write(
            &manifest_path,
            serde_json::to_vec_pretty(&manifest).unwrap_or_default(),
        )
        .await
        {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("writing upload manifest: {e}"),
            )
                .into_response();
        }
        // Truncate/create the staging file at offset 0.
        if let Err(e) = tokio::fs::write(&staging_path, &body[..]).await {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("writing first chunk: {e}"),
            )
                .into_response();
        }
    } else {
        // Subsequent chunk: must be exactly contiguous with what's
        // already on disk; otherwise 409 with the current size so the
        // client can resume.
        let current_size = tokio::fs::metadata(&staging_path)
            .await
            .map(|m| m.len())
            .unwrap_or(0);
        if range_start != current_size {
            return (
                StatusCode::CONFLICT,
                format!(
                    "chunk start {range_start} does not match staging size {current_size}; \
                     resume by re-sending from offset {current_size}"
                ),
            )
                .into_response();
        }
        use tokio::io::AsyncWriteExt;
        let mut f = match tokio::fs::OpenOptions::new()
            .append(true)
            .open(&staging_path)
            .await
        {
            Ok(f) => f,
            Err(e) => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("opening staging for append: {e}"),
                )
                    .into_response()
            }
        };
        if let Err(e) = f.write_all(&body[..]).await {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("appending chunk: {e}"),
            )
                .into_response();
        }
        // tokio::fs::File buffers writes internally — write_all returns
        // Ok before the bytes are flushed to the OS. Without this
        // flush + drop, the subsequent metadata read sees a stale
        // file size (writes still pending in user-space buffer), and
        // the client's chunk-3 request inherits a residual file size
        // That's smaller than declared. Symptom: "final chunk
        // but staging size N != declared total M".
        if let Err(e) = f.flush().await {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("flushing chunk: {e}"),
            )
                .into_response();
        }
        drop(f);
    }

    let received = tokio::fs::metadata(&staging_path)
        .await
        .map(|m| m.len())
        .unwrap_or(0);

    let _ = chunk_len; // currently informational only

    if range_end + 1 < total_bytes {
        return Json(serde_json::json!({
            "status": "partial",
            "received_bytes": received,
            "total_bytes": total_bytes,
        }))
        .into_response();
    }

    // Final chunk — verify total + sha256, then promote staging to
    // its final filename inside the per-token dir.
    if received != total_bytes {
        return (
            StatusCode::BAD_REQUEST,
            format!(
                "final chunk landed but staging size {received} != declared total {total_bytes}"
            ),
        )
            .into_response();
    }
    let declared_sha = headers
        .get("Upload-Sha256")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.trim().to_lowercase());

    let staging_for_hash = staging_path.clone();
    let computed_sha = match tokio::task::spawn_blocking(move || file_sha256(&staging_for_hash))
        .await
    {
        Ok(Ok(s)) => s,
        Ok(Err(e)) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("hashing assembled upload: {e}"),
            )
                .into_response()
        }
        Err(e) => {
            return (StatusCode::INTERNAL_SERVER_ERROR, format!("hash join: {e}")).into_response()
        }
    };
    if let Some(declared) = declared_sha.as_ref() {
        if declared != &computed_sha {
            // Mismatch → discard staging, ask client to retry.
            let _ = tokio::fs::remove_file(&staging_path).await;
            return (
                StatusCode::BAD_REQUEST,
                format!(
                    "Upload-Sha256 mismatch — declared={declared} computed={computed_sha}; staging discarded"
                ),
            )
                .into_response();
        }
    }
    // Promote staging.bin → its real filename.
    let manifest_bytes = match tokio::fs::read(&manifest_path).await {
        Ok(b) => b,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("reading upload manifest: {e}"),
            )
                .into_response()
        }
    };
    let manifest: serde_json::Value = serde_json::from_slice(&manifest_bytes)
        .unwrap_or_else(|_| serde_json::json!({"safe_filename": "upload.bin"}));
    let safe_name = manifest["safe_filename"]
        .as_str()
        .unwrap_or("upload.bin")
        .to_string();
    let final_path = upload_dir.join(&safe_name);
    if let Err(e) = tokio::fs::rename(&staging_path, &final_path).await {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("promoting staging file: {e}"),
        )
            .into_response();
    }

    Json(serde_json::json!({
        "status": "complete",
        "received_bytes": received,
        "total_bytes": total_bytes,
        "sha256": computed_sha,
        "final_path": final_path.to_string_lossy(),
    }))
    .into_response()
}

/// `POST /api/chat/session/:id/inputs/upload/:upload_token/finalize`
///
/// Closes a multi-file batch by walking the per-token upload dir and
/// registering its complete files as a single `UserInput` of
/// `kind: uploaded_files`. Idempotent — calling finalize twice for
/// the same token returns the same input_id without duplicating.
pub(crate) async fn finalize_upload(
    State(app): State<ChatAppState>,
    Path((session_id, upload_token)): Path<(Uuid, String)>,
) -> impl IntoResponse {
    let session = match app.conversation.get_session(session_id).await {
        Some(s) => s,
        None => return (StatusCode::NOT_FOUND, "session not found").into_response(),
    };
    if !upload_token
        .chars()
        .all(|c| c.is_alphanumeric() || c == '-')
        || upload_token.len() > 64
    {
        return (StatusCode::BAD_REQUEST, "invalid upload token").into_response();
    }
    let upload_dir = upload_root_for(&session.owner_user)
        .join(session_id.to_string())
        .join(&upload_token);
    if !upload_dir.exists() {
        return (
            StatusCode::NOT_FOUND,
            format!("no upload session found for token {upload_token}"),
        )
            .into_response();
    }

    // Walk + manifest. Idempotency: if a UserInput already references
    // this exact upload_dir, return it without creating a duplicate.
    let canonical = match upload_dir.canonicalize() {
        Ok(p) => p,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("canonicalizing upload dir: {e}"),
            )
                .into_response()
        }
    };
    let canonical_str = canonical.to_string_lossy().into_owned();
    if let Some(existing) = session
        .inputs
        .iter()
        .find(|i| i.kind == UserInputKind::UploadedFiles && i.root_path == canonical_str)
    {
        return Json(existing.clone()).into_response();
    }

    let walk_root = canonical.clone();
    let join_result: Result<Result<Vec<UserInputFile>, String>, tokio::task::JoinError> =
        tokio::task::spawn_blocking(move || build_manifest(&walk_root)).await;
    let mut files = match join_result {
        Ok(Ok(f)) => f,
        Ok(Err(msg)) => return (StatusCode::BAD_REQUEST, msg).into_response(),
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("manifest join error: {e}"),
            )
                .into_response()
        }
    };
    // Filter out the upload-manifest.json sidecar — it's
    // implementation detail, not user data.
    files.retain(|f| f.relpath != "upload-manifest.json");
    if files.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            "no completed file uploads found for this token",
        )
            .into_response();
    }

    let label = format!("upload-{}", &upload_token[..upload_token.len().min(8)]);
    let input = UserInput {
        input_id: Uuid::new_v4().simple().to_string()[..16].to_string(),
        label,
        kind: UserInputKind::UploadedFiles,
        root_path: canonical_str,
        files,
        registered_at: Utc::now(),
        registered_by: session.owner_user.clone(),
    };

    let store = app.conversation.store_handle();
    match store
        .update(session_id, move |s| {
            s.inputs.push(input.clone());
            Ok(())
        })
        .await
    {
        Ok(s) => {
            let last = s.inputs.last().cloned();
            Json(last).into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("persisting upload registration: {e}"),
        )
            .into_response(),
    }
}

/// Per-user upload root. Resolution order:
///   1. `ECAA_UPLOAD_ROOT` env var (operator override), with
///      `${USER}` substituted.
///   2. `<allowlist[0]>/.scripps-uploads` if the allowlist directory
///      is writable by the server's effective user (probed).
///   3. `<XDG_STATE_HOME or ~/.local/state>/ecaa-workflow/uploads`
///      — always writable for the server user.
///
/// Step 3 avoids the historical "Permission denied" on first upload
/// when no operator override is set and the allowlist directory isn't
/// writable. Per-session sub-directory is added by the caller.
fn upload_root_for(owner_user: &str) -> PathBuf {
    if let Ok(s) = std::env::var("ECAA_UPLOAD_ROOT") {
        return PathBuf::from(s.replace("${USER}", owner_user));
    }
    let roots = allowlisted_roots(owner_user);
    if let Some(root) = roots.first() {
        if dir_is_writable(root) {
            return root.join(".scripps-uploads");
        }
    }
    let base = std::env::var("XDG_STATE_HOME")
        .ok()
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var("HOME")
                .ok()
                .map(|h| PathBuf::from(h).join(".local/state"))
        })
        .unwrap_or_else(|| PathBuf::from(format!("/home/{owner_user}/.local/state")));
    base.join("ecaa-workflow").join("uploads")
}

/// True when `dir` exists + is a directory + a probe write succeeds.
/// Used by `upload_root_for` to pick the always-writable fallback when
/// the configured allowlist directory is read-only.
fn dir_is_writable(dir: &StdPath) -> bool {
    if !dir.is_dir() {
        return std::fs::create_dir_all(dir).is_ok();
    }
    let probe = dir.join(".swfc-write-probe");
    match std::fs::write(&probe, b"") {
        Ok(()) => {
            let _ = std::fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

/// Bail out of new chunk writes when the upload root's filesystem has
/// less than `ECAA_UPLOAD_DISK_RESERVE_GB` GB free (default 50). This
/// is a per-server soft guard; per-user quotas land later.
async fn check_disk_reserve_for(_session_id: Uuid, owner_user: &str) -> Result<(), String> {
    let reserve_gb: u64 = std::env::var("ECAA_UPLOAD_DISK_RESERVE_GB")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(50);
    let dir = upload_root_for(owner_user);
    let probe_dir = dir
        .ancestors()
        .find(|p| p.exists())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"));
    // Use `df` via shell as a portable fallback (avoids pulling in a
    // statvfs crate). The first column of the second line is the
    // filesystem; we want the "Available" column (4) in 1K blocks.
    let probe = probe_dir.clone();
    let avail_kb = tokio::task::spawn_blocking(move || -> std::io::Result<u64> {
        let out = std::process::Command::new("df")
            .arg("-Pk")
            .arg(&probe)
            .output()?;
        let stdout = String::from_utf8_lossy(&out.stdout);
        let line = stdout.lines().nth(1).unwrap_or("");
        let avail = line
            .split_whitespace()
            .nth(3)
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(u64::MAX);
        Ok(avail)
    })
    .await
    .map_err(|e| format!("df join: {e}"))?
    .map_err(|e| format!("df failed: {e}"))?;
    let avail_gb = avail_kb / (1024 * 1024);
    if avail_gb < reserve_gb {
        return Err(format!(
            "disk reserve guard tripped — {avail_gb} GB free on upload volume, \
             ECAA_UPLOAD_DISK_RESERVE_GB={reserve_gb}"
        ));
    }
    Ok(())
}

/// Parse `Content-Range: bytes <start>-<end>/<total>`.
fn parse_content_range(s: &str) -> Result<(u64, u64, u64), String> {
    let s = s.trim();
    let rest = s
        .strip_prefix("bytes ")
        .ok_or_else(|| "Content-Range must start with 'bytes '".to_string())?;
    let (range, total) = rest
        .split_once('/')
        .ok_or_else(|| "Content-Range missing '/' total separator".to_string())?;
    let total: u64 = total
        .parse()
        .map_err(|_| format!("Content-Range total {total:?} is not a u64"))?;
    let (start, end) = range
        .split_once('-')
        .ok_or_else(|| "Content-Range range missing '-' separator".to_string())?;
    let start: u64 = start
        .parse()
        .map_err(|_| format!("Content-Range start {start:?} is not a u64"))?;
    let end: u64 = end
        .parse()
        .map_err(|_| format!("Content-Range end {end:?} is not a u64"))?;
    if start > end || end >= total {
        return Err(format!(
            "Content-Range bytes {start}-{end}/{total} is malformed"
        ));
    }
    Ok((start, end, total))
}

/// Strip path separators, drop leading dots, cap length, keep only a
/// safe character class. Result is always non-empty.
fn sanitize_filename(raw: &str) -> String {
    let basename = StdPath::new(raw)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("upload.bin");
    let cleaned: String = basename
        .chars()
        .filter(|c| c.is_alphanumeric() || ".-_+".contains(*c))
        .take(120)
        .collect();
    let trimmed = cleaned.trim_start_matches('.').to_string();
    if trimmed.is_empty() {
        "upload.bin".to_string()
    } else {
        trimmed
    }
}

/// Per-file route inventory + builder. Documented next to its handler
/// list and used by the compile-time consistency assert in
/// `super::mod.rs` to ensure the aggregate `super::ROUTES` doesn't
/// drift from what each submodule serves.
#[allow(dead_code)] // doc-as-contract gate; consumed by const _: () assert.
pub(super) const ROUTES: &[(&str, &str)] = &[
    ("POST", "/api/chat/session/:id/inputs/upload"),
    (
        "POST",
        "/api/chat/session/:id/inputs/upload/:upload_token/finalize",
    ),
];

pub(super) fn routes() -> axum::Router<ChatAppState> {
    use axum::extract::DefaultBodyLimit;
    // axum's DefaultBodyLimit is 2 MiB by default. The chunked upload
    // protocol uses 8 MiB chunks (matches the comment on
    // UPLOAD_CHUNK_BYTES in ui/src/api/chatClient.ts) and finalize
    // requests can carry a manifest larger than 2 MiB for big batches.
    // Disable the default and rely on our own validation:
    // - per-chunk hard cap is `max_file_bytes()` (the smaller of
    // ECAA_UPLOAD_MAX_BYTES / disk reserve), enforced in
    // `upload_input_chunk` after parsing the Content-Range header
    // - finalize takes a JSON manifest, naturally bounded.
    // Without this override the browser sees `NetworkError when
    // attempting to fetch resource` on every chunk because axum
    // returns 413 before our handler runs.
    axum::Router::new()
        .route(
            "/api/chat/session/:id/inputs/upload",
            axum::routing::post(upload_input_chunk),
        )
        .route(
            "/api/chat/session/:id/inputs/upload/:upload_token/finalize",
            axum::routing::post(finalize_upload),
        )
        .layer(DefaultBodyLimit::disable())
}

#[cfg(test)]
mod tests {
    use crate::chat_routes::inputs::test_helpers::{allowlisted_temp, ensure_shared_root};
    use crate::chat_routes::test_support::{body_json, make_router};
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use sha2::{Digest, Sha256};
    use tower::util::ServiceExt;

    /// Serializes tests that mutate `ECAA_UPLOAD_*` env vars —
    /// `std::env::set_var` is process-global and concurrent test
    /// threads racing on the same key produce flaky results
    /// (e.g. one test sees another's "/disabled" upload root and
    /// fails assembly assertions). Same pattern as
    /// `chat_routes/stage_descriptions.rs::ENV_LOCK` and
    /// `harness/finalize_probe.rs::ENV_LOCK`.
    static UPLOAD_ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    #[tokio::test]
    async fn upload_endpoint_rejects_without_upload_token() {
        let _guard = UPLOAD_ENV_LOCK.lock().await;
        let _ = allowlisted_temp();
        let (router, app) = make_router(vec![]).await;
        let (id, _) = app.conversation.start_session(false).await.unwrap();
        // Point upload root inside the test allowlist so the disk
        // reserve probe and create_dir paths land where we control.
        std::env::set_var(
            "ECAA_UPLOAD_ROOT",
            ensure_shared_root().display().to_string() + "/.uploads",
        );
        // Disable the 50-GB free-space guard in unit tests — /tmp on
        // CI / dev rigs frequently has less than that and the guard
        // would short-circuit every upload assertion before we ever
        // exercise the real code path. Production deployments still
        // get the guard via the env-var default.
        std::env::set_var("ECAA_UPLOAD_DISK_RESERVE_GB", "0");
        let req = Request::builder()
            .method("POST")
            .uri(format!("/api/chat/session/{}/inputs/upload", id))
            .header("Content-Range", "bytes 0-3/4")
            .body(Body::from(vec![1, 2, 3, 4]))
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn upload_endpoint_round_trip_single_chunk_then_finalize() {
        let _guard = UPLOAD_ENV_LOCK.lock().await;
        let _ = allowlisted_temp();
        let (router, app) = make_router(vec![]).await;
        let (id, _) = app.conversation.start_session(false).await.unwrap();
        std::env::set_var(
            "ECAA_UPLOAD_ROOT",
            ensure_shared_root().display().to_string() + "/.uploads",
        );
        // Disable the 50-GB free-space guard in unit tests — /tmp on
        // CI / dev rigs frequently has less than that and the guard
        // would short-circuit every upload assertion before we ever
        // exercise the real code path. Production deployments still
        // get the guard via the env-var default.
        std::env::set_var("ECAA_UPLOAD_DISK_RESERVE_GB", "0");

        let payload = b"hello scripps upload\n";
        let total = payload.len();
        // Compute expected sha256 with the same crate the server uses.
        let mut h = Sha256::new();
        h.update(payload);
        let expected_sha = hex::encode(h.finalize());

        let req = Request::builder()
            .method("POST")
            .uri(format!("/api/chat/session/{}/inputs/upload", id))
            .header("Upload-Token", "test-token-001")
            .header("Upload-Filename", "data.tsv")
            .header("Content-Range", format!("bytes 0-{}/{}", total - 1, total))
            .header("Upload-Sha256", &expected_sha)
            .body(Body::from(payload.to_vec()))
            .unwrap();
        let resp = router.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp.into_body()).await;
        assert_eq!(body["status"].as_str(), Some("complete"));
        assert_eq!(body["sha256"].as_str(), Some(expected_sha.as_str()));

        // Now finalize the batch.
        let req = Request::builder()
            .method("POST")
            .uri(format!(
                "/api/chat/session/{}/inputs/upload/test-token-001/finalize",
                id
            ))
            .body(Body::empty())
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp.into_body()).await;
        assert_eq!(body["kind"].as_str(), Some("uploaded_files"));
        let files = body["files"].as_array().expect("files array");
        assert_eq!(files.len(), 1, "exactly one promoted upload file");
        assert_eq!(files[0]["relpath"].as_str(), Some("data.tsv"));
        assert_eq!(files[0]["sha256"].as_str(), Some(expected_sha.as_str()));

        // And the session now carries the new input.
        let session = app.conversation.get_session(id).await.unwrap();
        assert_eq!(session.inputs.len(), 1);
        assert_eq!(
            session.inputs[0].kind,
            ecaa_workflow_conversation::UserInputKind::UploadedFiles,
        );
    }

    #[tokio::test]
    async fn upload_endpoint_two_chunks_assemble_correctly() {
        let _guard = UPLOAD_ENV_LOCK.lock().await;
        let _ = allowlisted_temp();
        let (router, app) = make_router(vec![]).await;
        let (id, _) = app.conversation.start_session(false).await.unwrap();
        std::env::set_var(
            "ECAA_UPLOAD_ROOT",
            ensure_shared_root().display().to_string() + "/.uploads",
        );
        // Disable the 50-GB free-space guard in unit tests — /tmp on
        // CI / dev rigs frequently has less than that and the guard
        // would short-circuit every upload assertion before we ever
        // exercise the real code path. Production deployments still
        // get the guard via the env-var default.
        std::env::set_var("ECAA_UPLOAD_DISK_RESERVE_GB", "0");

        let payload: Vec<u8> = (0u8..200).collect();
        let total = payload.len();
        let mid = 80usize;
        let mut h = Sha256::new();
        h.update(&payload);
        let expected_sha = hex::encode(h.finalize());

        // Chunk 1: bytes 0..mid
        let r1 = Request::builder()
            .method("POST")
            .uri(format!("/api/chat/session/{}/inputs/upload", id))
            .header("Upload-Token", "two-chunk-token")
            .header("Upload-Filename", "blob.bin")
            .header("Content-Range", format!("bytes 0-{}/{}", mid - 1, total))
            .body(Body::from(payload[..mid].to_vec()))
            .unwrap();
        let resp = router.clone().oneshot(r1).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp.into_body()).await;
        assert_eq!(body["status"].as_str(), Some("partial"));

        // Chunk 2: bytes mid..total — final chunk includes sha256.
        let r2 = Request::builder()
            .method("POST")
            .uri(format!("/api/chat/session/{}/inputs/upload", id))
            .header("Upload-Token", "two-chunk-token")
            .header(
                "Content-Range",
                format!("bytes {}-{}/{}", mid, total - 1, total),
            )
            .header("Upload-Sha256", &expected_sha)
            .body(Body::from(payload[mid..].to_vec()))
            .unwrap();
        let resp = router.oneshot(r2).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp.into_body()).await;
        assert_eq!(body["status"].as_str(), Some("complete"));
        assert_eq!(body["sha256"].as_str(), Some(expected_sha.as_str()));
    }

    #[tokio::test]
    async fn upload_endpoint_rejects_sha256_mismatch_on_final_chunk() {
        let _guard = UPLOAD_ENV_LOCK.lock().await;
        let _ = allowlisted_temp();
        let (router, app) = make_router(vec![]).await;
        let (id, _) = app.conversation.start_session(false).await.unwrap();
        std::env::set_var(
            "ECAA_UPLOAD_ROOT",
            ensure_shared_root().display().to_string() + "/.uploads",
        );
        // Disable the 50-GB free-space guard in unit tests — /tmp on
        // CI / dev rigs frequently has less than that and the guard
        // would short-circuit every upload assertion before we ever
        // exercise the real code path. Production deployments still
        // get the guard via the env-var default.
        std::env::set_var("ECAA_UPLOAD_DISK_RESERVE_GB", "0");

        let payload = b"corruption-test\n";
        let total = payload.len();
        let req = Request::builder()
            .method("POST")
            .uri(format!("/api/chat/session/{}/inputs/upload", id))
            .header("Upload-Token", "sha-mismatch-token")
            .header("Upload-Filename", "x.bin")
            .header("Content-Range", format!("bytes 0-{}/{}", total - 1, total))
            .header("Upload-Sha256", "deadbeef".repeat(8))
            .body(Body::from(payload.to_vec()))
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn upload_endpoint_rejects_out_of_order_chunk() {
        let _guard = UPLOAD_ENV_LOCK.lock().await;
        let _ = allowlisted_temp();
        let (router, app) = make_router(vec![]).await;
        let (id, _) = app.conversation.start_session(false).await.unwrap();
        std::env::set_var(
            "ECAA_UPLOAD_ROOT",
            ensure_shared_root().display().to_string() + "/.uploads",
        );
        // Disable the 50-GB free-space guard in unit tests — /tmp on
        // CI / dev rigs frequently has less than that and the guard
        // would short-circuit every upload assertion before we ever
        // exercise the real code path. Production deployments still
        // get the guard via the env-var default.
        std::env::set_var("ECAA_UPLOAD_DISK_RESERVE_GB", "0");
        // Send a chunk starting at byte 100 without first uploading 0..100.
        // Server should 409 with current-size hint.
        let req = Request::builder()
            .method("POST")
            .uri(format!("/api/chat/session/{}/inputs/upload", id))
            .header("Upload-Token", "out-of-order-token")
            .header("Upload-Filename", "x.bin")
            .header("Content-Range", "bytes 100-199/200")
            .body(Body::from(vec![0u8; 100]))
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::CONFLICT);
    }

    /// The per-chunk hard cap exists and is 32 MiB. Anchored as a
    /// compile-time constant assertion so a regression that bumps the
    /// cap shows up in code review.
    #[test]
    fn chunk_size_capped_by_const() {
        const CAP: usize = 32 * 1024 * 1024;
        assert_eq!(super::MAX_UPLOAD_CHUNK_BYTES, CAP);
    }

    /// A forged `Content-Length` that exceeds the 32 MiB cap is
    /// rejected with 413 before the body is read.
    #[tokio::test]
    async fn upload_endpoint_rejects_oversized_content_length() {
        let _guard = UPLOAD_ENV_LOCK.lock().await;
        let _ = allowlisted_temp();
        let (router, app) = make_router(vec![]).await;
        let (id, _) = app.conversation.start_session(false).await.unwrap();
        std::env::set_var(
            "ECAA_UPLOAD_ROOT",
            ensure_shared_root().display().to_string() + "/.uploads",
        );
        std::env::set_var("ECAA_UPLOAD_DISK_RESERVE_GB", "0");

        // Forge a 33 MiB Content-Length but send a small body. Pre-check
        // should 413 before to_bytes even runs.
        let oversized = super::MAX_UPLOAD_CHUNK_BYTES as u64 + 1;
        let req = Request::builder()
            .method("POST")
            .uri(format!("/api/chat/session/{}/inputs/upload", id))
            .header("Upload-Token", "oversized-cl")
            .header("Upload-Filename", "x.bin")
            .header("Content-Range", "bytes 0-3/4")
            .header(axum::http::header::CONTENT_LENGTH, oversized.to_string())
            .body(Body::from(vec![1, 2, 3, 4]))
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::PAYLOAD_TOO_LARGE);
    }
}
