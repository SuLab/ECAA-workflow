//! `GET /api/chat/session/:id/package.tar.gz` — stream a gzipped
//! tarball of the emitted package directory as an HTTP attachment.
//!
//! Path-jail: the tarball root is `session.emitted_package_path`
//! (canonicalized). Returns 404 when the session has no emitted
//! package or the path no longer exists on disk.
//!
//! ## Streaming pipeline
//!
//! ```text
//!   spawn_blocking thread                       axum response body
//!   ─────────────────────                       ──────────────────
//!   tar::Builder<                                Body::from_stream(
//!     GzEncoder<                                   ReceiverStream(rx)
//!       BufWriter<                                 )
//!         ChannelWriter         ──── mpsc ───►
//!       >                            bounded
//!     >                              (~512KB
//!   >                                 buffered)
//! ```
//!
//! Each tar/gzip write hits a `BufWriter` (64KB internal buffer) that
//! batches the encoder's small writes before they reach the
//! `ChannelWriter`. ChannelWriter's `write()` does a `blocking_send`
//! on the mpsc, which back-pressures the producer thread when the
//! network is slow — peak server-side memory is O(channel_capacity ×
//! chunk_size) ≈ 512 KB regardless of archive size. A 5 GB package
//! downloads through the same memory budget as a 50 MB one.
//!
//! Bytes start flowing to the client within ~100 ms of the request
//! (just the time to spawn the thread + walk the first file), instead
//! of the multi-second wait the prior in-memory build imposed before
//! responding.
//!
//! Content-Length is intentionally absent — we don't know the
//! compressed size ahead of time. HTTP/1.1 chunked transfer encoding
//! handles framing; browsers + curl + wget all show "unknown size"
//! progress bars cleanly. If a precise size estimate is ever needed,
//! the archive can be pre-walked to sum file sizes + add a 5%
//! compression-ratio fudge — not worth the latency cost today.
//!
//! Error semantics:
//! - 404 on missing session / missing package root (caught before any
//!   bytes are sent).
//! - Mid-stream tar/IO errors: the channel closes, the client sees an
//!   incomplete-but-valid-gzip-prefix EOF. There's no clean way to
//!   surface a 500 once headers + body have started flowing; we log
//!   the producer-side error via tracing for forensics.
//! - Client disconnect: ChannelWriter detects the broken pipe and
//!   the producer thread exits early (no orphan thread).

use std::io::{self, Write as _};

use axum::{
    body::Body,
    extract::{Path, State},
    http::{header, StatusCode},
    response::{IntoResponse, Response},
};
use flate2::write::GzEncoder;
use flate2::Compression;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tokio_util::bytes::Bytes;
use uuid::Uuid;

use crate::chat_routes::app_state::ChatAppState;

/// Bytes per channel slot. The BufWriter above ChannelWriter is sized
/// to match; gzip's internal block size is 32 KB, so 64 KB chunks
/// usually carry 1-2 full gzip blocks each.
const CHUNK_SIZE: usize = 64 * 1024;

/// Channel slot count. 8 × 64 KB = 512 KB max buffered bytes between
/// the producer thread and the response stream. Bigger slack helps if
/// the network occasionally stalls; too big wastes RAM per concurrent
/// download.
const CHANNEL_CAPACITY: usize = 8;

/// Synchronous `io::Write` that forwards bytes into an async mpsc
/// channel as `Bytes` chunks. `blocking_send` provides back-pressure
/// when the receiver is slower than the producer — the blocking
/// thread parks instead of allocating an unbounded buffer.
struct ChannelWriter {
    tx: mpsc::Sender<Result<Bytes, io::Error>>,
}

impl io::Write for ChannelWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        let chunk = Bytes::copy_from_slice(buf);
        match self.tx.blocking_send(Ok(chunk)) {
            Ok(()) => Ok(buf.len()),
            Err(_) => Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "client disconnected before archive completed",
            )),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        // The BufWriter above us is the buffering layer; ChannelWriter
        // itself has no internal buffer to drain.
        Ok(())
    }
}

/// Producer body for the gzip-tar download stream: walks `root` under
/// `basename`, streaming through BufWriter → GzEncoder → tar → ChannelWriter →
/// `tx`. Any error is surfaced as a single error item on the channel before
/// EOF; the channel closes (and the response stream EOFs) when `tx` drops.
fn produce_package_tar_stream(
    tx: mpsc::Sender<Result<Bytes, io::Error>>,
    basename: &str,
    root: &std::path::Path,
) {
    let writer = io::BufWriter::with_capacity(CHUNK_SIZE, ChannelWriter { tx: tx.clone() });
    let enc = GzEncoder::new(writer, Compression::default());
    let mut tar = tar::Builder::new(enc);
    tar.follow_symlinks(false);

    // Surface any error through the channel so the response stream emits a
    // single error item before EOF; ChannelWriter already returns BrokenPipe
    // on disconnect, these branches cover the tar/gzip-side error cases.
    if let Err(e) = tar.append_dir_all(basename, root) {
        tracing::warn!(
            target: "package_download",
            error = %e,
            root = %root.display(),
            "tar walk failed mid-stream; closing channel — client will see truncated archive"
        );
        let _ = tx.blocking_send(Err(io::Error::other(e)));
        return;
    }
    let enc = match tar.into_inner() {
        Ok(e) => e,
        Err(e) => return fail_tar_stream(&tx, e, "tar finalize failed"),
    };
    let mut buf_writer = match enc.finish() {
        Ok(w) => w,
        Err(e) => return fail_tar_stream(&tx, e, "gzip finalize failed"),
    };
    if let Err(e) = buf_writer.flush() {
        tracing::warn!(
            target: "package_download",
            error = %e,
            "BufWriter final flush failed"
        );
    }
    // Drop tx by leaving scope; ReceiverStream sees EOF.
}

/// Log a tar/gzip finalize failure and surface it as a single channel error
/// item so the response stream emits the error before EOF.
fn fail_tar_stream(tx: &mpsc::Sender<Result<Bytes, io::Error>>, e: io::Error, msg: &str) {
    tracing::warn!(
        target: "package_download",
        error = %e,
        "{}",
        msg
    );
    let _ = tx.blocking_send(Err(io::Error::other(e)));
}

pub(crate) async fn get_package_tarball(
    State(app): State<ChatAppState>,
    Path(session_id): Path<Uuid>,
) -> impl IntoResponse {
    let Some(session) = app.conversation.get_session(session_id).await else {
        return (StatusCode::NOT_FOUND, "session not found").into_response();
    };
    let Some(root) = session.emitted_package_path.clone() else {
        return (
            StatusCode::NOT_FOUND,
            "session has no emitted package — confirm + emit first",
        )
            .into_response();
    };
    let Ok(root_canon) = root.canonicalize() else {
        return (StatusCode::NOT_FOUND, "package root missing on disk").into_response();
    };
    if !root_canon.is_dir() {
        return (StatusCode::NOT_FOUND, "package root is not a directory").into_response();
    }

    let archive_basename = root_canon
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("scripps-package")
        .to_string();
    let download_name = format!("{archive_basename}.tar.gz");

    // Build the stream pipeline: producer thread writes through
    // BufWriter → GzEncoder → tar → ChannelWriter → mpsc → ReceiverStream.
    let (tx, rx) = mpsc::channel::<Result<Bytes, io::Error>>(CHANNEL_CAPACITY);

    let root_for_thread = root_canon.clone();
    let basename_for_thread = archive_basename.clone();
    tokio::task::spawn_blocking(move || {
        produce_package_tar_stream(tx, &basename_for_thread, &root_for_thread);
    });

    let stream = ReceiverStream::new(rx);
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/gzip")
        .header(
            header::CONTENT_DISPOSITION,
            format!("attachment; filename=\"{download_name}\""),
        )
        .header(header::CACHE_CONTROL, "private, no-store")
        // Hint to the proxy layer that streaming is preferred; nginx
        // / cloudfront default to buffering responses unless told
        // otherwise. (No-op when the chat server is exposed
        // directly, but harmless.)
        .header("x-accel-buffering", "no")
        .body(Body::from_stream(stream))
        .unwrap_or_else(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("response build failed: {e}"),
            )
                .into_response()
        })
}

/// `GET /api/chat/session/:id/deposit-package.zip` — stream a clean,
/// deposit-ready `.zip` of the emitted package.
///
/// Mirrors [`get_package_tarball`]'s session→`emitted_package_path`
/// resolution + path-jail (canonicalize + `is_dir`) and inherits its
/// owner-auth from the `/session/:id/` URL shape. Where the tarball handler
/// streams the *full working tree*, this one runs the core exporter
/// ([`export_depositable_package`]) to copy only the tier A+B
/// audit/review/deposit + re-execution surface into a scratch dir, re-seal
/// BagIt + RO-Crate, and strip `.git`; it then zips that clean tree into a
/// `NamedTempFile` and streams the file as `application/zip`.
///
/// The export + zip run on a blocking thread (filesystem-heavy, synchronous
/// `zip` writer). The scratch dir + temp zip are deleted when their guards
/// drop after the response body has been fully read — `ReaderStream` holds
/// an open `tokio::fs::File`, and the `NamedTempFile` guard is moved into a
/// closure that runs on stream completion so the bytes outlive the producer.
pub(crate) async fn get_deposit_package(
    State(app): State<ChatAppState>,
    Path(session_id): Path<Uuid>,
) -> impl IntoResponse {
    let Some(session) = app.conversation.get_session(session_id).await else {
        return (StatusCode::NOT_FOUND, "session not found").into_response();
    };
    let Some(root) = session.emitted_package_path.clone() else {
        return (
            StatusCode::NOT_FOUND,
            "session has no emitted package — confirm + emit first",
        )
            .into_response();
    };
    let Ok(root_canon) = root.canonicalize() else {
        return (StatusCode::NOT_FOUND, "package root missing on disk").into_response();
    };
    if !root_canon.is_dir() {
        return (StatusCode::NOT_FOUND, "package root is not a directory").into_response();
    }

    let pkg_basename = root_canon
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("scripps-package")
        .to_string();
    let download_name = format!("{pkg_basename}-deposit.zip");

    // Export + zip are synchronous, filesystem-heavy work: run on a blocking
    // thread so the async runtime isn't parked. The result is a sealed temp
    // `.zip` file we then stream.
    let zip_result = tokio::task::spawn_blocking(move || build_deposit_zip(&root_canon)).await;
    let zip_tempfile = match zip_result {
        Ok(Ok(f)) => f,
        Ok(Err(e)) => {
            tracing::warn!(
                target: "package_download",
                error = %e,
                "depositable export failed"
            );
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "depositable export failed",
            )
                .into_response();
        }
        Err(e) => {
            tracing::warn!(
                target: "package_download",
                error = %e,
                "depositable export task panicked"
            );
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "depositable export task failed",
            )
                .into_response();
        }
    };

    // Re-open the temp zip as an independent OS handle for streaming, then
    // drop the `NamedTempFile` guard. On unix the guard's `unlink` only
    // removes the directory entry; the inode (and its bytes) stays live as
    // long as our re-opened fd is held — `ReaderStream` keeps that fd open
    // until the body reaches EOF, at which point the file is reclaimed. This
    // mirrors the established temp-then-stream pattern without holding the
    // guard across the async boundary.
    let std_file = match zip_tempfile.reopen() {
        Ok(f) => f,
        Err(e) => {
            tracing::warn!(
                target: "package_download",
                error = %e,
                "reopening deposit zip tempfile failed"
            );
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "deposit zip read failed",
            )
                .into_response();
        }
    };
    drop(zip_tempfile);
    let async_file = tokio::fs::File::from_std(std_file);
    let stream = tokio_util::io::ReaderStream::new(async_file);

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/zip")
        .header(
            header::CONTENT_DISPOSITION,
            format!("attachment; filename=\"{download_name}\""),
        )
        .header(header::CACHE_CONTROL, "private, no-store")
        .header("x-accel-buffering", "no")
        .body(Body::from_stream(stream))
        .unwrap_or_else(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("response build failed: {e}"),
            )
                .into_response()
        })
}

/// Export the tier-A+B surface of `root` into a scratch dir and zip it into a
/// sealed `NamedTempFile`. Runs on a blocking thread (see caller). The scratch
/// `TempDir` is dropped (and deleted) before returning; the returned
/// `NamedTempFile` carries the finished `.zip`.
fn build_deposit_zip(root: &std::path::Path) -> anyhow::Result<tempfile::NamedTempFile> {
    use anyhow::Context as _;
    use ecaa_workflow_core::emitter::{export_depositable_package, zip_dir};

    let staging = tempfile::tempdir().context("creating deposit export staging dir")?;
    let export_root = staging.path().join("export");
    export_depositable_package(root, &export_root)
        .with_context(|| format!("exporting package {}", root.display()))?;

    let mut zip_file = tempfile::NamedTempFile::new().context("creating deposit zip tempfile")?;
    zip_dir(&export_root, zip_file.as_file_mut()).context("zipping deposit export")?;
    // `zip_dir`'s `ZipWriter::finish()` flushed the archive; the caller
    // streams via a fresh `reopen()` handle (offset 0), so no rewind here.
    Ok(zip_file)
}

#[cfg(test)]
mod tests {
    use crate::chat_routes::test_support::{make_router, seed_session_with_completed_task};
    use axum::body::{to_bytes, Body};
    use axum::http::{Request, StatusCode};
    use tower::util::ServiceExt;
    use uuid::Uuid;

    #[tokio::test]
    async fn returns_404_for_unknown_session() {
        let (router, _) = make_router(vec![]).await;
        let bogus = Uuid::new_v4();
        let resp = router
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/api/chat/session/{}/package.tar.gz", bogus))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn returns_404_when_session_has_no_emitted_package() {
        let (router, app) = make_router(vec![]).await;
        let sid = seed_session_with_completed_task(&app, "t_demo", None).await;
        let resp = router
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/api/chat/session/{}/package.tar.gz", sid))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn returns_tarball_with_expected_headers_and_entries() {
        // Build a stub package directory + emit a session that points
        // at it; download and verify the archive parses + carries the
        // expected entries.
        let tmp = tempfile::tempdir().unwrap();
        let pkg_root = tmp.path().join("alpha-bulk_rnaseq-20260601T000000");
        std::fs::create_dir_all(&pkg_root).unwrap();
        std::fs::write(pkg_root.join("WORKFLOW.json"), b"{\"workflow_id\":\"t\"}").unwrap();
        std::fs::write(pkg_root.join("PROMPT.md"), b"prompt").unwrap();
        std::fs::create_dir_all(pkg_root.join("runtime/outputs/task_a")).unwrap();
        std::fs::write(
            pkg_root.join("runtime/outputs/task_a/result.json"),
            b"{\"status\":\"completed\"}",
        )
        .unwrap();

        let (router, app) = make_router(vec![]).await;
        let sid = seed_session_with_completed_task(&app, "t_demo", Some(pkg_root.clone())).await;
        let resp = router
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/api/chat/session/{}/package.tar.gz", sid))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        // Verify headers.
        let headers = resp.headers().clone();
        assert_eq!(
            headers
                .get(axum::http::header::CONTENT_TYPE)
                .unwrap()
                .to_str()
                .unwrap(),
            "application/gzip"
        );
        let dispo = headers
            .get(axum::http::header::CONTENT_DISPOSITION)
            .unwrap()
            .to_str()
            .unwrap();
        assert!(
            dispo.contains("alpha-bulk_rnaseq-20260601T000000.tar.gz"),
            "disposition should carry archive name; got {dispo}"
        );

        // Decode the body and verify entries.
        let body = resp.into_body();
        let bytes = to_bytes(body, 64 * 1024 * 1024).await.unwrap();
        let gz = flate2::read::GzDecoder::new(&bytes[..]);
        let mut tar_reader = tar::Archive::new(gz);
        let mut entry_paths: Vec<String> = Vec::new();
        for entry in tar_reader.entries().unwrap() {
            let entry = entry.unwrap();
            entry_paths.push(entry.path().unwrap().to_string_lossy().into_owned());
        }
        let has_workflow = entry_paths.iter().any(|p| p.ends_with("/WORKFLOW.json"));
        let has_prompt = entry_paths.iter().any(|p| p.ends_with("/PROMPT.md"));
        let has_result = entry_paths
            .iter()
            .any(|p| p.contains("runtime/outputs/task_a/result.json"));
        assert!(
            has_workflow,
            "WORKFLOW.json missing from tarball: {entry_paths:?}"
        );
        assert!(
            has_prompt,
            "PROMPT.md missing from tarball: {entry_paths:?}"
        );
        assert!(
            has_result,
            "task_a/result.json missing from tarball: {entry_paths:?}"
        );
    }

    #[tokio::test]
    async fn streams_large_archive_without_buffering_in_memory() {
        // Regression for the streaming contract: a multi-MB package
        // must download through the streaming pipeline with peak
        // server-side memory bounded by CHUNK_SIZE * CHANNEL_CAPACITY.
        // We can't directly measure RSS in a unit test, but we can
        // verify that:
        //   1. The response body decodes to the expected total size
        //      (not silently truncated by a small buffer).
        //   2. Content-Length header is absent (streaming responses
        //      use chunked transfer encoding instead).
        //   3. The archive contains all entries even for sizes well
        //      beyond CHUNK_SIZE * CHANNEL_CAPACITY (~512 KB).
        let tmp = tempfile::tempdir().unwrap();
        let pkg_root = tmp.path().join("stream-test-pkg-20260601T000000");
        std::fs::create_dir_all(&pkg_root).unwrap();
        // Generate ~4 MB of synthetic data across multiple files; well
        // above the channel's buffered capacity so the streaming
        // back-pressure path actually exercises.
        std::fs::write(pkg_root.join("WORKFLOW.json"), b"{}").unwrap();
        for i in 0..8 {
            let payload = vec![b'x'; 512 * 1024]; // 512 KB each = 4 MB total
            std::fs::write(pkg_root.join(format!("task_{:02}.bin", i)), &payload).unwrap();
        }

        let (router, app) = make_router(vec![]).await;
        let sid = seed_session_with_completed_task(&app, "t_demo", Some(pkg_root.clone())).await;
        let resp = router
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/api/chat/session/{}/package.tar.gz", sid))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        // Content-Length must be absent — streaming responses don't
        // know the compressed size ahead of time.
        let headers = resp.headers().clone();
        assert!(
            headers.get(axum::http::header::CONTENT_LENGTH).is_none(),
            "streaming response should not declare Content-Length"
        );
        // Transfer encoding: hyper's response builder adds
        // `transfer-encoding: chunked` automatically when no
        // Content-Length is set. Either explicit chunked OR no
        // length-related headers at all is acceptable (axum may
        // strip transfer-encoding in some configurations).
        // Streaming hint for proxies — keeps nginx / cloudfront from
        // re-buffering the response, which would defeat the point.
        assert_eq!(
            headers
                .get("x-accel-buffering")
                .and_then(|v| v.to_str().ok()),
            Some("no"),
            "X-Accel-Buffering hint should disable proxy buffering"
        );

        // Read the whole body and verify it decompresses to the full
        // expected entry set. `to_bytes` polls the stream to EOF.
        let body = resp.into_body();
        let bytes = to_bytes(body, 32 * 1024 * 1024).await.unwrap();
        // Compressed archive should be substantial (incompressible
        // would be > ~4 MB; 'xxxx...' compresses to a few KB but the
        // pipeline still ran).
        assert!(
            !bytes.is_empty(),
            "streamed body should not be empty (got 0 bytes)"
        );

        // Decode + count entries.
        let gz = flate2::read::GzDecoder::new(&bytes[..]);
        let mut tar_reader = tar::Archive::new(gz);
        let mut entry_count = 0usize;
        let mut total_uncompressed = 0u64;
        for entry in tar_reader.entries().unwrap() {
            let entry = entry.unwrap();
            entry_count += 1;
            total_uncompressed += entry.header().size().unwrap_or(0);
        }
        // 1 WORKFLOW.json + 8 task_*.bin + the directory entry itself.
        assert!(
            entry_count >= 9,
            "archive should carry all 9 files; got {entry_count} entries"
        );
        // Total uncompressed payload should be at least ~4 MB
        // (8 × 512 KB) — confirms nothing got dropped mid-stream.
        assert!(
            total_uncompressed >= 8 * 512 * 1024,
            "uncompressed total {} below expected 4 MB — pipeline may have truncated",
            total_uncompressed
        );
    }

    #[tokio::test]
    async fn deposit_package_returns_zip_without_git_or_cache() {
        // Build a stub emitted package carrying a tier-A result table plus
        // tier-E `.git`/cache entries, point a session at it, and download
        // the depositable `.zip`. Assert 200 application/zip whose body
        // opens as a valid archive that keeps the tier-A file but drops the
        // `.git` + cache entries.
        let tmp = tempfile::tempdir().unwrap();
        let pkg_root = tmp.path().join("alpha-bulk_rnaseq-20260601T000000");
        std::fs::create_dir_all(pkg_root.join("runtime/outputs/task_a")).unwrap();
        std::fs::write(
            pkg_root.join("runtime/outputs/task_a/result.json"),
            b"{\"status\":\"completed\"}",
        )
        .unwrap();
        // Tier-A crate substrate so the export tree is well-formed.
        std::fs::write(
            pkg_root.join("ro-crate-metadata.json"),
            br#"{"@context":"https://w3id.org/ro/crate/1.1/context","@graph":[{"@id":"ro-crate-metadata.json","@type":"CreativeWork","about":{"@id":"./"}},{"@id":"./","@type":"Dataset","hasPart":[]}]}"#,
        )
        .unwrap();
        // Tier-E bloat that MUST be stripped from the deposit.
        std::fs::create_dir_all(pkg_root.join(".git")).unwrap();
        std::fs::write(pkg_root.join(".git/config"), b"[core]\n").unwrap();
        std::fs::create_dir_all(pkg_root.join("runtime/cache/x")).unwrap();
        std::fs::write(pkg_root.join("runtime/cache/x/y"), b"cached\n").unwrap();
        // Tier-C operational log that MUST be stripped.
        std::fs::write(
            pkg_root.join("runtime/outputs/task_a/agent-claude.log"),
            b"log line\n",
        )
        .unwrap();

        let (router, app) = make_router(vec![]).await;
        let sid = seed_session_with_completed_task(&app, "t_demo", Some(pkg_root.clone())).await;
        let resp = router
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/api/chat/session/{}/deposit-package.zip", sid))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::OK,
            "deposit-package.zip should return 200"
        );

        // Headers: application/zip + disposition naming the deposit zip.
        let headers = resp.headers().clone();
        assert_eq!(
            headers
                .get(axum::http::header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok()),
            Some("application/zip"),
            "deposit download must be application/zip"
        );
        let dispo = headers
            .get(axum::http::header::CONTENT_DISPOSITION)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        assert!(
            dispo.contains("alpha-bulk_rnaseq-20260601T000000-deposit.zip"),
            "disposition should carry the deposit zip name; got {dispo}"
        );

        // Body opens as a valid zip lacking .git/cache entries.
        let body = resp.into_body();
        let bytes = to_bytes(body, 64 * 1024 * 1024).await.unwrap();
        let cursor = std::io::Cursor::new(bytes.to_vec());
        let mut archive =
            zip::ZipArchive::new(cursor).expect("deposit body must open as a zip archive");
        let names: Vec<String> = (0..archive.len())
            .map(|i| archive.by_index(i).unwrap().name().to_string())
            .collect();

        assert!(
            names
                .iter()
                .any(|n| n == "runtime/outputs/task_a/result.json"),
            "tier-A result.json must survive into the deposit zip; entries: {names:?}"
        );
        assert!(
            !names.iter().any(|n| n.starts_with(".git/")),
            "deposit zip must NOT carry any .git/ entry; entries: {names:?}"
        );
        assert!(
            !names.iter().any(|n| n.starts_with("runtime/cache/")),
            "deposit zip must NOT carry any runtime/cache/ entry; entries: {names:?}"
        );
        assert!(
            !names.iter().any(|n| n.ends_with(".log")),
            "deposit zip must NOT carry any *.log entry; entries: {names:?}"
        );
    }

    #[tokio::test]
    async fn deposit_package_returns_404_for_unknown_session() {
        let (router, _) = make_router(vec![]).await;
        let bogus = Uuid::new_v4();
        let resp = router
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/api/chat/session/{}/deposit-package.zip", bogus))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }
}
