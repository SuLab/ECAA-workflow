//! Upload + import of an externally-obtained ECAA package. Extracts a
//! `.zip`/`.tar.gz` under a path-jail, validates it's an ECAA package,
//! reconstructs a read-only `Session`, and exposes a capability probe.
//!
//! No new LLM `Tool` — import + capabilities are deterministic HTTP
//! endpoints like confirm/replay. Imported sessions are strictly
//! read-only: `ensure_not_imported` gates every lifecycle mutation
//! (branch/amend/rerun/emit/start-execution).

use std::io::Read;
use std::path::{Path, PathBuf};

use axum::extract::{Request, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use ecaa_workflow_conversation::Session;
use ecaa_workflow_core::package_import::{probe_package_capabilities, PackageCapabilities};
use futures_util::StreamExt;
use serde::Serialize;
use uuid::Uuid;

use super::{assert_under_root, safe_relative_join, ChatAppState};

const MAGIC_ZIP: [u8; 4] = [0x50, 0x4B, 0x03, 0x04];
const MAGIC_GZIP: [u8; 2] = [0x1F, 0x8B];

fn bad_request(msg: impl Into<String>) -> (StatusCode, String) {
    (StatusCode::BAD_REQUEST, msg.into())
}

/// RAII cleanup for import staging + extraction paths. While `armed`, every
/// tracked path is removed on `Drop` — so a partially-extracted `imported/
/// <token>/` dir (or the staging `.bin`) is reclaimed on *every* early exit,
/// including a client disconnect that drops the `spawn_blocking().await`
/// future mid-flight. Disarm the dest guard only once the imported session is
/// durably saved; staging is never kept. `Drop` uses `let _ =` so a
/// double-remove (path already gone) never panics.
struct CleanupPaths {
    paths: Vec<PathBuf>,
    armed: bool,
}

impl Drop for CleanupPaths {
    fn drop(&mut self) {
        if self.armed {
            for p in &self.paths {
                if p.is_dir() {
                    let _ = std::fs::remove_dir_all(p);
                } else {
                    let _ = std::fs::remove_file(p);
                }
            }
        }
    }
}

// ────────────────────────────────────────────────────────────────────────────
// Archive extraction (path-jailed)
// ────────────────────────────────────────────────────────────────────────────

/// Sniff the archive format by leading magic bytes and dispatch to the right
/// extractor. Every entry is path-jailed under `dest`; symlink entries and
/// traversal are rejected; `max_entries` bounds a zip bomb by count and
/// `max_extracted_bytes` bounds a decompression bomb by total decompressed
/// size (the compressed-upload cap can't bound how large a tiny archive
/// expands to).
pub(super) fn extract_archive(
    archive: &Path,
    dest: &Path,
    max_entries: usize,
    max_extracted_bytes: u64,
) -> Result<(), (StatusCode, String)> {
    let mut head = [0u8; 4];
    {
        let mut f = std::fs::File::open(archive)
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("open archive: {e}")))?;
        let _ = f.read(&mut head);
    }
    std::fs::create_dir_all(dest)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("mkdir dest: {e}")))?;

    if head == MAGIC_ZIP {
        extract_zip(archive, dest, max_entries, max_extracted_bytes)
    } else if head[..2] == MAGIC_GZIP {
        extract_targz(archive, dest, max_entries, max_extracted_bytes)
    } else {
        Err(bad_request(
            "unrecognized archive format (expected .zip or .tar.gz)",
        ))
    }
}

fn extract_zip(
    archive: &Path,
    dest: &Path,
    max_entries: usize,
    max_extracted_bytes: u64,
) -> Result<(), (StatusCode, String)> {
    let file = std::fs::File::open(archive)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("open zip: {e}")))?;
    let mut zip = zip::ZipArchive::new(file).map_err(|e| bad_request(format!("bad zip: {e}")))?;
    if zip.len() > max_entries {
        return Err(bad_request(format!(
            "archive has too many entries ({})",
            zip.len()
        )));
    }
    let mut total_extracted: u64 = 0;
    for i in 0..zip.len() {
        let mut entry = zip
            .by_index(i)
            .map_err(|e| bad_request(format!("zip entry {i}: {e}")))?;
        // Reject symlink entries — a symlink written into the tree could later
        // be followed out of the jail by a downstream reader.
        if let Some(mode) = entry.unix_mode() {
            const S_IFLNK: u32 = 0o120000;
            if mode & 0o170000 == S_IFLNK {
                return Err(bad_request("archive contains a symlink entry"));
            }
        }
        // `enclosed_name` already blocks traversal; path-jail is the belt.
        let Some(rel) = entry.enclosed_name() else {
            return Err(bad_request("archive entry escapes root"));
        };
        let out =
            safe_relative_join(dest, &rel).map_err(|e| bad_request(format!("path jail: {e}")))?;
        if entry.is_dir() {
            std::fs::create_dir_all(&out)
                .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("mkdir: {e}")))?;
            continue;
        }
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("mkdir parent: {e}")))?;
        }
        let mut w = std::fs::File::create(&out)
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("create file: {e}")))?;
        // Bounded copy: cap the running decompressed total so a tiny archive
        // can't expand to terabytes and fill the host disk. The `+1` lets a
        // single over-cap entry push `total_extracted` past the cap so the
        // check below fires even on the last allowed byte.
        let remaining = max_extracted_bytes.saturating_sub(total_extracted);
        let mut limited = std::io::Read::take(&mut entry, remaining + 1);
        let n = std::io::copy(&mut limited, &mut w)
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("write file: {e}")))?;
        total_extracted += n;
        if total_extracted > max_extracted_bytes {
            return Err((
                StatusCode::PAYLOAD_TOO_LARGE,
                "extracted size exceeds cap".into(),
            ));
        }
        assert_under_root(dest, &out).map_err(|e| bad_request(format!("escaped root: {e}")))?;
    }
    Ok(())
}

fn extract_targz(
    archive: &Path,
    dest: &Path,
    max_entries: usize,
    max_extracted_bytes: u64,
) -> Result<(), (StatusCode, String)> {
    let file = std::fs::File::open(archive)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("open tar.gz: {e}")))?;
    let gz = flate2::read::GzDecoder::new(file);
    let mut tar = tar::Archive::new(gz);
    let mut count = 0usize;
    let mut total_extracted: u64 = 0;
    for entry in tar
        .entries()
        .map_err(|e| bad_request(format!("bad tar: {e}")))?
    {
        let mut entry = entry.map_err(|e| bad_request(format!("tar entry: {e}")))?;
        count += 1;
        if count > max_entries {
            return Err(bad_request("archive has too many entries"));
        }
        let etype = entry.header().entry_type();
        if etype.is_symlink() || etype.is_hard_link() {
            return Err(bad_request("archive contains a symlink entry"));
        }
        let rel = entry
            .path()
            .map_err(|e| bad_request(format!("tar path: {e}")))?
            .into_owned();
        let out =
            safe_relative_join(dest, &rel).map_err(|e| bad_request(format!("path jail: {e}")))?;
        if etype.is_dir() {
            std::fs::create_dir_all(&out)
                .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("mkdir: {e}")))?;
            continue;
        }
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("mkdir parent: {e}")))?;
        }
        let mut w = std::fs::File::create(&out)
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("create file: {e}")))?;
        // Bounded copy: cap the running decompressed total (decompression-bomb
        // defense). See `extract_zip` for the `+1` rationale.
        let remaining = max_extracted_bytes.saturating_sub(total_extracted);
        let mut limited = std::io::Read::take(&mut entry, remaining + 1);
        let n = std::io::copy(&mut limited, &mut w)
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("write file: {e}")))?;
        total_extracted += n;
        if total_extracted > max_extracted_bytes {
            return Err((
                StatusCode::PAYLOAD_TOO_LARGE,
                "extracted size exceeds cap".into(),
            ));
        }
        assert_under_root(dest, &out).map_err(|e| bad_request(format!("escaped root: {e}")))?;
    }
    Ok(())
}

/// Both download endpoints nest the package under a top-level `<basename>/`
/// dir, so after extraction the crate root may be one level down. Return the
/// dir that actually contains `WORKFLOW.json`.
pub(super) fn locate_package_root(extracted: &Path) -> Option<PathBuf> {
    if extracted.join("WORKFLOW.json").exists() {
        return Some(extracted.to_path_buf());
    }
    let dirs: Vec<PathBuf> = std::fs::read_dir(extracted)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    if dirs.len() == 1 && dirs[0].join("WORKFLOW.json").exists() {
        return Some(dirs[0].clone());
    }
    None
}

// ────────────────────────────────────────────────────────────────────────────
// Endpoints
// ────────────────────────────────────────────────────────────────────────────

pub(super) const ROUTES: &[(&str, &str)] = &[
    ("POST", "/api/chat/package/import"),
    ("GET", "/api/chat/session/:id/capabilities"),
];

pub(super) fn routes() -> axum::Router<ChatAppState> {
    use axum::extract::DefaultBodyLimit;
    axum::Router::new()
        .route(
            "/api/chat/package/import",
            axum::routing::post(import_package),
        )
        .route(
            "/api/chat/session/:id/capabilities",
            axum::routing::get(get_capabilities),
        )
        // Import is a single-shot streaming upload, size-capped inside the
        // handler by `app.config.max_import_bytes`; disable the default 2 MiB
        // body limit so large packages reach the streaming reader.
        .layer(DefaultBodyLimit::disable())
}

#[derive(Serialize)]
struct ImportResponse {
    session_id: Uuid,
    imported: bool,
    capabilities: PackageCapabilities,
}

#[derive(Serialize)]
struct CapabilitiesResponse {
    imported: bool,
    capabilities: PackageCapabilities,
}

/// Stream the request body to `path`, aborting with 413 if it exceeds
/// `max_bytes`. Single-shot: the whole archive is staged on disk before
/// extraction so the sync extractor can run under `spawn_blocking`.
async fn stream_body_to_file(
    request: Request,
    path: &Path,
    max_bytes: u64,
) -> Result<(), Response> {
    let mut file = tokio::fs::File::create(path).await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("create staging: {e}"),
        )
            .into_response()
    })?;
    let mut total: u64 = 0;
    let mut stream = request.into_body().into_data_stream();
    while let Some(chunk) = stream.next().await {
        let bytes = chunk
            .map_err(|e| (StatusCode::BAD_REQUEST, format!("read body: {e}")).into_response())?;
        total += bytes.len() as u64;
        if total > max_bytes {
            let _ = tokio::fs::remove_file(path).await;
            return Err((
                StatusCode::PAYLOAD_TOO_LARGE,
                format!("archive exceeds {max_bytes} bytes"),
            )
                .into_response());
        }
        tokio::io::AsyncWriteExt::write_all(&mut file, &bytes)
            .await
            .map_err(|e| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("write staging: {e}"),
                )
                    .into_response()
            })?;
    }
    tokio::io::AsyncWriteExt::flush(&mut file).await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("flush staging: {e}"),
        )
            .into_response()
    })?;
    Ok(())
}

/// `POST /api/chat/package/import` — upload an ECAA package archive
/// (`.zip`/`.tar.gz`), extract + validate + probe it, reconstruct a
/// read-only `Session`, and return `{ session_id, imported, capabilities }`.
#[tracing::instrument(skip(app, headers, request))]
pub(super) async fn import_package(
    State(app): State<ChatAppState>,
    headers: axum::http::HeaderMap,
    request: Request,
) -> Response {
    // `owner_user_from_headers` returns `Option<String>` (None on the
    // anonymous/CLI path). We stamp it onto the reconstructed `Session`
    // *before* the save below so ownership is atomic with persistence — a
    // failed owner stamp can never leave the imported package at the
    // world-readable "local" sentinel (fail-open).
    let owner_user = super::sessions::owner_user_from_headers(&headers);
    let import_root = app.config.package_root.join("imported");
    let staging_dir = import_root.join(".staging");
    if let Err(e) = tokio::fs::create_dir_all(&staging_dir).await {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("mkdir staging: {e}"),
        )
            .into_response();
    }
    let token = Uuid::new_v4().to_string();
    let staging_path = staging_dir.join(format!("{token}.bin"));

    // Staging is never kept — this guard reclaims the `.bin` on every exit
    // path (stream error, extraction failure, save failure, and — crucially —
    // a client disconnect that drops the `spawn_blocking().await` future).
    // It is never disarmed.
    let _staging_cleanup = CleanupPaths {
        paths: vec![staging_path.clone()],
        armed: true,
    };

    if let Err(resp) =
        stream_body_to_file(request, &staging_path, app.config.max_import_bytes).await
    {
        return resp;
    }

    let dest = import_root.join(&token);
    let max_entries = app.config.max_import_entries;
    let max_extracted = app.config.max_import_extracted_bytes;
    let staging_for_task = staging_path.clone();
    let dest_for_task = dest.clone();

    // The extracted `imported/<token>/` dir is removed on every failure path
    // (locate miss, missing ro-crate, reconstruct error, join error, save
    // error, cancellation) and disarmed only once the session is durably
    // saved on the success path below.
    let mut dest_cleanup = CleanupPaths {
        paths: vec![dest.clone()],
        armed: true,
    };

    let joined = tokio::task::spawn_blocking(
        move || -> Result<(Session, PackageCapabilities), (StatusCode, String)> {
            extract_archive(&staging_for_task, &dest_for_task, max_entries, max_extracted)?;
            let root = locate_package_root(&dest_for_task).ok_or_else(|| {
                (
                    StatusCode::BAD_REQUEST,
                    "not an ECAA package (missing WORKFLOW.json)".to_string(),
                )
            })?;
            if !root.join("ro-crate-metadata.json").exists() {
                return Err((
                    StatusCode::BAD_REQUEST,
                    "not an ECAA package (missing ro-crate-metadata.json)".to_string(),
                ));
            }
            let caps = probe_package_capabilities(&root);
            let session = Session::from_imported_package(&root)
                .map_err(|e| (StatusCode::UNPROCESSABLE_ENTITY, format!("reconstruct: {e}")))?;
            Ok((session, caps))
        },
    )
    .await;

    // Prompt async removal of staging on the normal path; `_staging_cleanup`
    // is the belt that also fires on cancellation. Double-remove is harmless.
    let _ = tokio::fs::remove_file(&staging_path).await;

    match joined {
        Ok(Ok((mut session, capabilities))) => {
            // Owner-before-save: stamp ownership atomically with persistence so
            // a header-derived owner is never lost to the "local" sentinel.
            if let Some(u) = owner_user {
                session.owner_user = u;
            }
            let id = session.id;
            if let Err(e) = app.conversation.store_handle().save(&session).await {
                // dest_cleanup stays armed → the extracted dir is reclaimed.
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("save session: {e}"),
                )
                    .into_response();
            }
            // Save succeeded — the extracted package is now a durable session.
            dest_cleanup.armed = false;
            Json(ImportResponse {
                session_id: id,
                imported: true,
                capabilities,
            })
            .into_response()
        }
        Ok(Err((code, msg))) => (code, msg).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, format!("join: {e}")).into_response(),
    }
}

/// `GET /api/chat/session/:id/capabilities` — re-probe an emitted package's
/// completeness. Works for both imported and locally-created emitted sessions.
#[tracing::instrument(skip(app), fields(session_id = %session_id))]
pub(super) async fn get_capabilities(
    State(app): State<ChatAppState>,
    axum::extract::Path(session_id): axum::extract::Path<Uuid>,
) -> Response {
    let Some(session) = app.conversation.get_session(session_id).await else {
        return (StatusCode::NOT_FOUND, "session not found").into_response();
    };
    let Some(root) = session.emitted_package_path.clone() else {
        return (StatusCode::NOT_FOUND, "package not yet emitted").into_response();
    };
    let caps = tokio::task::spawn_blocking(move || probe_package_capabilities(&root))
        .await
        .unwrap_or_else(|_| probe_package_capabilities(Path::new("/nonexistent")));
    Json(CapabilitiesResponse {
        imported: session.imported,
        capabilities: caps,
    })
    .into_response()
}

// ────────────────────────────────────────────────────────────────────────────
// Read-only guard
// ────────────────────────────────────────────────────────────────────────────

/// Reject an action that mutates lifecycle on a read-only imported package.
/// Applied after the session is fetched in every lifecycle handler
/// (start_execution / branch / amend / rerun).
pub(crate) fn ensure_not_imported(session: &Session) -> Result<(), Response> {
    if session.imported {
        return Err((
            StatusCode::PRECONDITION_FAILED,
            "this action is not available for imported (read-only) packages",
        )
            .into_response());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn zip_with(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut buf = std::io::Cursor::new(Vec::new());
        {
            let mut zw = zip::ZipWriter::new(&mut buf);
            let opts = zip::write::SimpleFileOptions::default();
            for (name, data) in entries {
                zw.start_file(*name, opts).unwrap();
                zw.write_all(data).unwrap();
            }
            zw.finish().unwrap();
        }
        buf.into_inner()
    }

    #[test]
    fn extracts_flat_zip() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("a.zip");
        std::fs::write(&archive, zip_with(&[("WORKFLOW.json", b"{}")])).unwrap();
        let dest = dir.path().join("out");
        extract_archive(&archive, &dest, 1000, 1 << 30).unwrap();
        assert!(dest.join("WORKFLOW.json").exists());
    }

    #[test]
    fn rejects_zip_slip() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("evil.zip");
        std::fs::write(&archive, zip_with(&[("../escape.txt", b"pwned")])).unwrap();
        let dest = dir.path().join("out");
        let err = extract_archive(&archive, &dest, 1000, 1 << 30).unwrap_err();
        assert_eq!(err.0, axum::http::StatusCode::BAD_REQUEST);
        assert!(!dir.path().join("escape.txt").exists());
    }

    #[test]
    fn extract_rejects_decompression_bomb() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("bomb.zip");
        // A single entry whose *uncompressed* size far exceeds the tiny cap we
        // pass — the compressed archive on disk is tiny (highly-repetitive
        // data deflates well), which is exactly the decompression-bomb shape.
        let payload = vec![b'A'; 4096];
        std::fs::write(&archive, zip_with(&[("WORKFLOW.json", &payload)])).unwrap();
        let dest = dir.path().join("out");
        let err = extract_archive(&archive, &dest, 1000, 256).unwrap_err();
        assert_eq!(err.0, axum::http::StatusCode::PAYLOAD_TOO_LARGE);
    }

    #[test]
    fn cleanup_paths_removes_when_armed_keeps_when_disarmed() {
        let dir = tempfile::tempdir().unwrap();

        // Armed: both a dir and a file are reclaimed on drop.
        let armed_dir = dir.path().join("armed");
        std::fs::create_dir_all(&armed_dir).unwrap();
        std::fs::write(armed_dir.join("nested"), b"x").unwrap();
        let armed_file = dir.path().join("armed.bin");
        std::fs::write(&armed_file, b"y").unwrap();
        {
            let _g = CleanupPaths {
                paths: vec![armed_dir.clone(), armed_file.clone()],
                armed: true,
            };
        }
        assert!(!armed_dir.exists(), "armed guard should remove the dir");
        assert!(!armed_file.exists(), "armed guard should remove the file");

        // Disarmed: the path survives.
        let kept_dir = dir.path().join("kept");
        std::fs::create_dir_all(&kept_dir).unwrap();
        {
            let _g = CleanupPaths {
                paths: vec![kept_dir.clone()],
                armed: false,
            };
        }
        assert!(kept_dir.exists(), "disarmed guard should keep the dir");

        // Double-remove (path already gone) must not panic.
        let gone = dir.path().join("never-existed");
        {
            let _g = CleanupPaths {
                paths: vec![gone.clone()],
                armed: true,
            };
        }
        assert!(!gone.exists());
    }

    #[test]
    fn locate_finds_nested_package_root() {
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("pkg-name");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(nested.join("WORKFLOW.json"), b"{}").unwrap();
        assert_eq!(locate_package_root(dir.path()), Some(nested));
    }

    #[test]
    fn locate_finds_flat_package_root() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("WORKFLOW.json"), b"{}").unwrap();
        assert_eq!(
            locate_package_root(dir.path()),
            Some(dir.path().to_path_buf())
        );
    }
}
