//! Per-task log surface: paginated progress.log, recursive log-file
//! listing, and a generalised tailer for the SME drawer's Logs tab.
//!
//! Endpoints (plan §S16.2):
//! - GET /session/:id/task/:task_id/progress-log
//! - GET /session/:id/task/:task_id/logs
//! - GET /session/:id/task/:task_id/log-tail
//!
//! Shares the recursive-walk helper `list_task_files` with `scripts.rs`
//! via the private `super::list_task_files` re-export. Tailer helpers
//! `empty_log_response` and `PROGRESS_LOG_MAX_BYTES` live in `mod.rs`
//! since both `progress-log` and `log-tail` need them.

use super::super::*;
use super::{empty_log_response, PROGRESS_LOG_MAX_BYTES};
use axum::{extract::State, http::StatusCode, response::IntoResponse, Json};
use std::collections::VecDeque;
use tokio::io::{AsyncBufReadExt, BufReader};
use uuid::Uuid;

/// Stream-tail a log file line-by-line, keeping at most `byte_cap`
/// bytes of tail in memory while counting `total_lines`.
///
/// Returns `(tail_lines, total_lines, skipped_prefix)` where
/// `skipped_prefix` is the count of lines dropped off the front of
/// the ring buffer to keep the byte budget. The previous shape used
/// `tokio::fs::read_to_string` and allocated the whole file before
/// truncating — a 5 GB agent log allocated 5 GB on every 2-second
/// UI poll (P1-98). This bound is O(byte_cap) regardless of file
/// size: we read line-by-line through a `BufReader` and evict the
/// oldest line whenever the byte total would exceed the cap.
async fn stream_tail_lines(
    path: &std::path::Path,
    byte_cap: usize,
) -> Result<(Vec<String>, usize, usize), std::io::Error> {
    let file = tokio::fs::File::open(path).await?;
    let mut reader = BufReader::new(file).lines();
    let mut ring: VecDeque<String> = VecDeque::new();
    let mut bytes = 0usize;
    let mut total = 0usize;
    let mut skipped_prefix = 0usize;
    while let Some(line) = reader.next_line().await? {
        total += 1;
        bytes += line.len() + 1;
        ring.push_back(line);
        while bytes > byte_cap && ring.len() > 1 {
            if let Some(dropped) = ring.pop_front() {
                bytes = bytes.saturating_sub(dropped.len() + 1);
                skipped_prefix += 1;
            }
        }
    }
    Ok((ring.into_iter().collect(), total, skipped_prefix))
}

/// Resolve the jailed `runtime/outputs/<task_id>/progress.log` path.
/// Returns `BAD_REQUEST` on any traversal/separator violation.
fn resolve_progress_log(
    pkg: &std::path::Path,
    task_id: &str,
) -> Result<std::path::PathBuf, StatusCode> {
    let task_dir = super::super::_path_jail::runtime_outputs_for_task(pkg, task_id)
        .map_err(|_| StatusCode::BAD_REQUEST)?;
    Ok(task_dir.join("progress.log"))
}

/// Resolve `<pkg>/runtime/outputs/<task_id>/<sub>` for the log-tail
/// endpoint. Jails both `task_id` and the sub-path, then asserts the
/// joined path stays under `pkg`.
fn resolve_log_tail_path(
    pkg: &std::path::Path,
    task_id: &str,
    sub: &str,
) -> Result<std::path::PathBuf, StatusCode> {
    let task_dir = super::super::_path_jail::runtime_outputs_for_task(pkg, task_id)
        .map_err(|_| StatusCode::BAD_REQUEST)?;
    // The sub path may contain a single nested segment (e.g. "scripts/foo.log").
    // We require it not to traverse upward, but we DO need to walk multiple
    // components because logs are commonly under sub-dirs.
    let rel = std::path::Path::new(sub.trim_start_matches('/'));
    let target = super::super::_path_jail::safe_relative_join(&task_dir, rel)
        .map_err(|_| StatusCode::BAD_REQUEST)?;
    super::super::_path_jail::assert_under_root(pkg, &target)
        .map_err(|_| StatusCode::BAD_REQUEST)?;
    Ok(target)
}

/// Resolve `<pkg>/runtime/outputs/<task_id>` for the recursive-walk
/// file listing. Same jail as the other resolvers.
fn resolve_task_dir_for_listing(
    pkg: &std::path::Path,
    task_id: &str,
) -> Result<std::path::PathBuf, StatusCode> {
    super::super::_path_jail::runtime_outputs_for_task(pkg, task_id)
        .map_err(|_| StatusCode::BAD_REQUEST)
}

/// Tail `runtime/outputs/<task_id>/progress.log` paginated by line.
///
/// Paginated via `?since_line=<n>` — client passes back the
/// `next_since_line` from the prior response to fetch only new lines,
/// avoiding quadratic growth when the drawer polls every 2s on a
/// long-running task.
///
/// Always 200. Missing file / empty file ⇒ empty lines array so the UI
/// renders a "no activity yet" placeholder rather than erroring out.
pub async fn get_progress_log(
    State(app): State<ChatAppState>,
    Path((session_id, task_id)): Path<(Uuid, String)>,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let Some(session) = app.conversation.get_session(session_id).await else {
        return (StatusCode::NOT_FOUND, "session not found").into_response();
    };
    let Some(pkg) = session.emitted_package_path.clone() else {
        return Json(serde_json::json!({
            "lines": [],
            "total_lines": 0,
            "next_since_line": 0,
            "truncated": false,
        }))
        .into_response();
    };
    let since: usize = params
        .get("since_line")
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let path = match resolve_progress_log(&pkg, &task_id) {
        Ok(p) => p,
        Err(_) => {
            // Treat a bad task_id as "no progress yet" so the UI doesn't
            // surface a hard error on a still-untriggered task.
            return Json(serde_json::json!({
                "lines": [],
                "total_lines": 0,
                "next_since_line": 0,
                "truncated": false,
            }))
            .into_response();
        }
    };

    let (tail, total, skipped_prefix) = match stream_tail_lines(&path, PROGRESS_LOG_MAX_BYTES).await
    {
        Ok(t) => t,
        Err(_) => {
            return Json(serde_json::json!({
                "lines": [],
                "total_lines": 0,
                "next_since_line": 0,
                "truncated": false,
            }))
            .into_response();
        }
    };
    // First-line index of the tail window in absolute terms; any
    // `since_line` below this points at a line the byte-cap dropped
    // off the front of the ring, so we surface what we have rather
    // than zero results.
    let tail_start = skipped_prefix;
    let start = since.min(total).max(tail_start);
    let drop_from_tail = start.saturating_sub(tail_start);
    let out: Vec<String> = tail.into_iter().skip(drop_from_tail).collect();
    let truncated = skipped_prefix > 0;
    let returned_count = out.len();
    let next_since_line = if truncated {
        total.saturating_sub(returned_count)
    } else {
        total
    };

    Json(serde_json::json!({
        "lines": out,
        "total_lines": total,
        "next_since_line": next_since_line,
        "truncated": truncated,
    }))
    .into_response()
}

/// List `runtime/outputs/<task_id>/**.{log,jsonl,txt,out}` (recursive
/// scan, depth-2 to keep responses bounded). Used by the Logs tab in
/// the per-task drawer so the SME can pick a log to tail beyond the
/// hard-coded `progress.log`.
pub async fn list_task_logs(
    State(app): State<ChatAppState>,
    Path((session_id, task_id)): Path<(Uuid, String)>,
) -> impl IntoResponse {
    list_task_files(app, session_id, &task_id, FileFilter::Logs).await
}

/// Generalised log tailer. Same response shape as `get_progress_log`
/// but accepts `?path=<rel-path>` jailed to the task's output dir.
/// The path is canonicalised against the package root so symlink
/// escapes return 403.
pub async fn get_task_log_tail(
    State(app): State<ChatAppState>,
    Path((session_id, task_id)): Path<(Uuid, String)>,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let Some(session) = app.conversation.get_session(session_id).await else {
        return (StatusCode::NOT_FOUND, "session not found").into_response();
    };
    let Some(pkg) = session.emitted_package_path.clone() else {
        return empty_log_response();
    };
    let Some(rel) = params.get("path") else {
        return (StatusCode::BAD_REQUEST, "missing ?path=").into_response();
    };
    let target = match resolve_log_tail_path(&pkg, &task_id, rel) {
        Ok(p) => p,
        Err(status) => return (status, "invalid task_id or path").into_response(),
    };
    let Ok(target_canon) = target.canonicalize() else {
        return empty_log_response();
    };
    // Belt-and-suspenders: canonicalized path must still sit under pkg.
    if super::super::_path_jail::assert_under_root(&pkg, &target_canon).is_err() {
        return (StatusCode::FORBIDDEN, "path escapes package root").into_response();
    }
    if !target_canon.is_file() {
        return empty_log_response();
    }
    let since: usize = params
        .get("since_line")
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let (tail, total, skipped_prefix) =
        match stream_tail_lines(&target_canon, PROGRESS_LOG_MAX_BYTES).await {
            Ok(t) => t,
            Err(_) => return empty_log_response(),
        };
    let tail_start = skipped_prefix;
    let start = since.min(total).max(tail_start);
    let drop_from_tail = start.saturating_sub(tail_start);
    let out: Vec<String> = tail.into_iter().skip(drop_from_tail).collect();
    let truncated = skipped_prefix > 0;
    let returned_count = out.len();
    let next_since_line = if truncated {
        total.saturating_sub(returned_count)
    } else {
        total
    };
    Json(serde_json::json!({
        "lines": out,
        "total_lines": total,
        "next_since_line": next_since_line,
        "truncated": truncated,
    }))
    .into_response()
}

/// Filter for [`list_task_files`]. Logs covers tail-friendly extensions;
/// Scripts (used by `scripts.rs`) covers the `R/py/sh/Rmd` set the SME
/// reviews in the per-task drawer's Scripts tab.
#[derive(Clone, Copy)]
pub(super) enum FileFilter {
    Logs,
    Scripts,
}

impl FileFilter {
    fn matches(&self, name: &str) -> bool {
        match self {
            FileFilter::Logs => {
                name.ends_with(".log")
                    || name.ends_with(".jsonl")
                    || name.ends_with(".txt")
                    || name.ends_with(".out")
            }
            FileFilter::Scripts => {
                name.ends_with(".R")
                    || name.ends_with(".py")
                    || name.ends_with(".sh")
                    || name.ends_with(".Rmd")
            }
        }
    }
}

/// Recursive-walk helper shared by `list_task_logs` and
/// `list_task_scripts`. Depth-bound at 2 so the response stays bounded
/// even for tasks that emit many sub-stage outputs.
pub(super) async fn list_task_files(
    app: ChatAppState,
    session_id: Uuid,
    task_id: &str,
    filter: FileFilter,
) -> axum::response::Response {
    let Some(session) = app.conversation.get_session(session_id).await else {
        return (StatusCode::NOT_FOUND, "session not found").into_response();
    };
    let Some(pkg) = session.emitted_package_path.clone() else {
        return Json(serde_json::json!({ "files": [] })).into_response();
    };
    let task_dir = match resolve_task_dir_for_listing(&pkg, task_id) {
        Ok(p) => p,
        Err(_) => return Json(serde_json::json!({ "files": [] })).into_response(),
    };
    let task_dir_canon = match task_dir.canonicalize() {
        Ok(p) => p,
        Err(_) => return Json(serde_json::json!({ "files": [] })).into_response(),
    };
    // Belt-and-suspenders: canonicalized task_dir must still sit under pkg.
    if super::super::_path_jail::assert_under_root(&pkg, &task_dir_canon).is_err() {
        return Json(serde_json::json!({ "files": [] })).into_response();
    }
    let mut out: Vec<serde_json::Value> = Vec::new();
    let mut stack: Vec<(std::path::PathBuf, u32)> = vec![(task_dir_canon.clone(), 0)];
    while let Some((dir, depth)) = stack.pop() {
        if depth > 2 {
            continue;
        }
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(meta) = entry.metadata() else { continue };
            let path = entry.path();
            if meta.is_dir() {
                stack.push((path, depth + 1));
                continue;
            }
            if !meta.is_file() {
                continue;
            }
            let name = match entry.file_name().into_string() {
                Ok(n) => n,
                Err(_) => continue,
            };
            if !filter.matches(&name) {
                continue;
            }
            let rel = match path.strip_prefix(&task_dir_canon) {
                Ok(p) => p.to_string_lossy().into_owned(),
                Err(_) => continue,
            };
            let mtime = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs())
                .unwrap_or(0);
            out.push(serde_json::json!({
                "rel_path": rel,
                "size_bytes": meta.len(),
                "mtime_unix": mtime,
            }));
        }
    }
    out.sort_by(|a, b| {
        b["mtime_unix"]
            .as_u64()
            .unwrap_or(0)
            .cmp(&a["mtime_unix"].as_u64().unwrap_or(0))
    });
    Json(serde_json::json!({ "files": out })).into_response()
}

pub(crate) fn routes() -> axum::Router<ChatAppState> {
    axum::Router::new()
        .route(
            "/api/chat/session/:id/task/:task_id/progress-log",
            axum::routing::get(get_progress_log),
        )
        .route(
            "/api/chat/session/:id/task/:task_id/logs",
            axum::routing::get(list_task_logs),
        )
        .route(
            "/api/chat/session/:id/task/:task_id/log-tail",
            axum::routing::get(get_task_log_tail),
        )
}

#[cfg(test)]
mod tests {
    use super::{resolve_log_tail_path, resolve_progress_log, resolve_task_dir_for_listing};
    use crate::chat_routes::test_support::{
        body_json, make_router, seed_session_with_completed_task,
    };
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::util::ServiceExt;
    use uuid::Uuid;

    #[test]
    fn progress_log_rejects_traversal() {
        let tmp = tempfile::tempdir().unwrap();
        let pkg = tmp.path();
        std::fs::create_dir_all(pkg.join("runtime/outputs/legit")).unwrap();
        assert!(matches!(
            resolve_progress_log(pkg, ".."),
            Err(StatusCode::BAD_REQUEST)
        ));
        assert!(matches!(
            resolve_progress_log(pkg, "../../etc"),
            Err(StatusCode::BAD_REQUEST)
        ));
    }

    #[test]
    fn progress_log_resolves_legit() {
        let tmp = tempfile::tempdir().unwrap();
        let pkg = tmp.path();
        std::fs::create_dir_all(pkg.join("runtime/outputs/legit")).unwrap();
        let path = resolve_progress_log(pkg, "legit").unwrap();
        assert_eq!(path, pkg.join("runtime/outputs/legit/progress.log"));
    }

    #[test]
    fn log_tail_rejects_traversal_task_id() {
        let tmp = tempfile::tempdir().unwrap();
        let pkg = tmp.path();
        std::fs::create_dir_all(pkg.join("runtime/outputs/legit")).unwrap();
        assert!(resolve_log_tail_path(pkg, "../../etc", "anything.log").is_err());
    }

    #[test]
    fn log_tail_rejects_traversal_subpath() {
        let tmp = tempfile::tempdir().unwrap();
        let pkg = tmp.path();
        std::fs::create_dir_all(pkg.join("runtime/outputs/legit")).unwrap();
        assert!(resolve_log_tail_path(pkg, "legit", "../../../etc/passwd").is_err());
    }

    #[test]
    fn list_task_dir_rejects_traversal() {
        let tmp = tempfile::tempdir().unwrap();
        let pkg = tmp.path();
        std::fs::create_dir_all(pkg.join("runtime/outputs/legit")).unwrap();
        assert!(matches!(
            resolve_task_dir_for_listing(pkg, ".."),
            Err(StatusCode::BAD_REQUEST)
        ));
    }

    #[tokio::test]
    async fn progress_log_returns_empty_when_session_has_no_package() {
        let (router, app) = make_router(vec![]).await;
        let id = seed_session_with_completed_task(&app, "t_demo", None).await;
        let req = Request::builder()
            .method("GET")
            .uri(format!("/api/chat/session/{}/task/t_demo/progress-log", id))
            .body(Body::empty())
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp.into_body()).await;
        assert_eq!(body["total_lines"], 0);
        assert_eq!(body["next_since_line"], 0);
        assert_eq!(body["truncated"], false);
        assert!(body["lines"].as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn progress_log_returns_all_lines_then_pages_via_since_line() {
        let tmp = tempfile::TempDir::new().unwrap();
        // Seed progress.log with 5 lines in the conventional location.
        let dir = tmp.path().join("runtime").join("outputs").join("t_demo");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("progress.log"),
            "line one\nline two\nline three\nline four\nline five\n",
        )
        .unwrap();

        let (router, app) = make_router(vec![]).await;
        let id =
            seed_session_with_completed_task(&app, "t_demo", Some(tmp.path().to_path_buf())).await;

        // First call returns all 5 lines.
        let req = Request::builder()
            .method("GET")
            .uri(format!("/api/chat/session/{}/task/t_demo/progress-log", id))
            .body(Body::empty())
            .unwrap();
        let resp = router.clone().oneshot(req).await.unwrap();
        let body = body_json(resp.into_body()).await;
        assert_eq!(body["total_lines"], 5);
        assert_eq!(body["next_since_line"], 5);
        let lines: Vec<String> = body["lines"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect();
        assert_eq!(
            lines,
            vec![
                "line one",
                "line two",
                "line three",
                "line four",
                "line five"
            ]
        );

        // Second call with since_line=5 returns no new lines (progress stalled).
        let req = Request::builder()
            .method("GET")
            .uri(format!(
                "/api/chat/session/{}/task/t_demo/progress-log?since_line=5",
                id
            ))
            .body(Body::empty())
            .unwrap();
        let resp = router.clone().oneshot(req).await.unwrap();
        let body = body_json(resp.into_body()).await;
        assert!(body["lines"].as_array().unwrap().is_empty());
        assert_eq!(body["total_lines"], 5);
        assert_eq!(body["next_since_line"], 5);

        // since_line=3 returns the last two lines only.
        let req = Request::builder()
            .method("GET")
            .uri(format!(
                "/api/chat/session/{}/task/t_demo/progress-log?since_line=3",
                id
            ))
            .body(Body::empty())
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        let body = body_json(resp.into_body()).await;
        assert_eq!(body["lines"].as_array().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn progress_log_caps_payload_to_128kb_and_reports_truncated() {
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path().join("runtime").join("outputs").join("t_big");
        std::fs::create_dir_all(&dir).unwrap();
        // 200 lines of 1 KB each = 200 KB, exceeds the 128 KB cap.
        let one_line = "x".repeat(1023);
        let mut content = String::new();
        for _ in 0..200 {
            content.push_str(&one_line);
            content.push('\n');
        }
        std::fs::write(dir.join("progress.log"), &content).unwrap();

        let (router, app) = make_router(vec![]).await;
        let id =
            seed_session_with_completed_task(&app, "t_big", Some(tmp.path().to_path_buf())).await;
        let req = Request::builder()
            .method("GET")
            .uri(format!("/api/chat/session/{}/task/t_big/progress-log", id))
            .body(Body::empty())
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        let body = body_json(resp.into_body()).await;
        assert_eq!(body["total_lines"], 200);
        assert_eq!(body["truncated"], true);
        let returned = body["lines"].as_array().unwrap().len();
        // 128 KB / ~1 KB per line = ~128 lines of tail.
        assert!(returned > 100 && returned < 140, "got {}", returned);
        // next_since_line should skip the truncated prefix so a
        // follow-up poll doesn't re-read dropped lines.
        let next_since = body["next_since_line"].as_u64().unwrap() as usize;
        assert_eq!(next_since, 200 - returned);
    }

    #[tokio::test]
    async fn progress_log_404_when_session_missing() {
        let (router, _) = make_router(vec![]).await;
        let bogus = Uuid::new_v4();
        let req = Request::builder()
            .method("GET")
            .uri(format!(
                "/api/chat/session/{}/task/whatever/progress-log",
                bogus
            ))
            .body(Body::empty())
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }
}
