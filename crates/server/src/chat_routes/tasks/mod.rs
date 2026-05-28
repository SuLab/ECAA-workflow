//! Per-task surface — read endpoints (results, logs, sentinels, blocker
//! info) and mutation endpoints (amend method, rerun, SME decisions,
//! undo amendment, impact preview, run wrapper scripts).
//!
//! Split from a single 2644-LOC `tasks.rs` into:
//! - `mod.rs` (this file) — thin re-export hub + `routes()` + `ROUTES`.
//! - `result.rs` — `get_task_result`, `get_artifact`,
//!   `get_active_tasks`, `get_stuck_tasks`, `post_task_note`, plus
//!   the artifact-cache + mime helpers (shared across the module).
//! - `blocker.rs` — `get_task_blocker`, `post_sme_decisions`,
//!   `post_sme_selection`, `auto_approve_discoveries`, plus
//!   `read_task_attempts`.
//! - `scripts.rs` — `list_task_scripts`, `post_rerun_script`.
//! - `logs.rs` — `get_progress_log`, `list_task_logs`,
//!   `get_task_log_tail`, plus the file-listing helper.
//! - `sentinels.rs` — `get_task_status_sentinels` and the
//!   `classify_status_filename` helper.
//! - `impact.rs` — `post_amend_method`, `post_rerun`,
//!   `post_undo_amendment`, `post_impact_preview` (the cluster of
//!   state-mutation handlers gated by `try_transition`).
//!
//! Tests stay co-located with the handlers they exercise.
//!
//! Cross-module helpers:
//! - `mime_for_path`, `config_dir_or_default`, `PROGRESS_LOG_MAX_BYTES`,
//!   `empty_log_response` live in this file (private) — used by both
//!   `result` and `logs`.

use super::ChatAppState;

pub(super) mod blocker;
pub(super) mod impact;
pub(super) mod logs;
pub(super) mod package_download;
pub(super) mod result;
pub(super) mod scripts;
pub(super) mod sentinels;
pub(super) mod task_state;

// Re-export the public handlers so callers that reach in via
// `chat_routes::tasks::<name>` keep resolving, and so
// `pub use chat_routes::tasks::{...}` in `chat_routes/mod.rs` is
// untouched.
pub use blocker::{
    auto_approve_discoveries, get_task_blocker, post_sme_decisions, post_sme_selection,
};
pub use impact::{post_amend_method, post_impact_preview, post_rerun, post_undo_amendment};
pub use logs::{get_progress_log, get_task_log_tail, list_task_logs};
pub use result::{
    get_active_tasks, get_artifact, get_stuck_tasks, get_task_result, post_task_note,
};
pub use scripts::{list_task_scripts, post_rerun_script};
pub use sentinels::get_task_status_sentinels;
// `task_state` handler is reachable through the `task_state::routes()`
// builder merged in `routes()` below — no external callers need the
// direct symbol, so we skip the otherwise-conventional `pub use`.

/// Route inventory for the doc-as-contract gate +
/// per-submodule `routes()` builder. `mod.rs::router()` merges every
/// submodule's builder into the single chat surface. The aggregate
/// here concatenates each per-file slice in display order.
pub(super) const ROUTES: &[(&str, &str)] = &[
    ("GET", "/api/chat/session/:id/task/:task_id/result"),
    ("GET", "/api/chat/session/:id/artifacts/*path"),
    ("GET", "/api/chat/session/:id/package.tar.gz"),
    ("POST", "/api/chat/session/:id/auto-approve-discoveries"),
    ("POST", "/api/chat/session/:id/task/:task_id/sme-selection"),
    ("GET", "/api/chat/session/:id/task/:task_id/progress-log"),
    (
        "GET",
        "/api/chat/session/:id/task/:task_id/status-sentinels",
    ),
    ("GET", "/api/chat/session/:id/task/:task_id/logs"),
    ("GET", "/api/chat/session/:id/task/:task_id/scripts"),
    ("GET", "/api/chat/session/:id/task/:task_id/log-tail"),
    ("POST", "/api/chat/session/:id/task/:task_id/rerun-script"),
    ("GET", "/api/chat/session/:id/stuck-tasks"),
    ("GET", "/api/chat/session/:id/active-tasks"),
    ("POST", "/api/chat/session/:id/task/:task_id/impact-preview"),
    ("GET", "/api/chat/session/:id/task/:task_id/blocker"),
    ("POST", "/api/chat/session/:id/task/:task_id/sme-decisions"),
    ("POST", "/api/chat/session/:id/task/:task_id/amend-method"),
    ("POST", "/api/chat/session/:id/task/:task_id/undo-amendment"),
    ("POST", "/api/chat/session/:id/task/:task_id/note"),
    ("POST", "/api/chat/session/:id/task/:task_id/rerun"),
    ("POST", "/api/chat/session/:id/task/:task_id/state"),
];

pub(super) fn routes() -> axum::Router<ChatAppState> {
    axum::Router::new()
        .merge(result::routes())
        .merge(blocker::routes())
        .merge(scripts::routes())
        .merge(logs::routes())
        .merge(sentinels::routes())
        .merge(impact::routes())
        .merge(task_state::routes())
}

// ── Cross-submodule private helpers ───────────────────────────────────

/// Default config directory resolution. Used by `result.rs` (verification
/// lookup) and once-elsewhere; lives here so both submodules import via
/// `super::config_dir_or_default()`.
pub(super) fn config_dir_or_default() -> std::path::PathBuf {
    std::env::var("ECAA_CONFIG_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::path::PathBuf::from("config"))
}

/// MIME mapping for the artifact-fetch + artifact-listing paths. Used
/// by `result.rs` (both `scan_artifacts` and `get_artifact`); kept here
/// so adding a new extension is a single-file edit.
pub(super) fn mime_for_path(p: &std::path::Path) -> &'static str {
    let ext = p
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "svg" => "image/svg+xml",
        "webp" => "image/webp",
        "html" | "htm" => "text/html; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "js" => "application/javascript",
        "json" => "application/json",
        "tsv" => "text/tab-separated-values",
        "csv" => "text/csv",
        "txt" | "log" | "md" => "text/plain; charset=utf-8",
        "pdf" => "application/pdf",
        _ => "application/octet-stream",
    }
}

/// Max bytes of progress.log returned in one response. 128 KB is
/// generous for typical agent runs (~500–2000 log lines per iteration)
/// but caps runaway logs so the HTTP payload stays bounded. Clients
/// that want more tail the file with `?since_line=N` pagination. Used
/// by both `progress-log` and `log-tail` endpoints in `logs.rs`.
pub(super) const PROGRESS_LOG_MAX_BYTES: usize = 128 * 1024;

/// Standard empty-log envelope used by both `progress-log` and
/// `log-tail` when the package, target file, or task dir is missing.
pub(super) fn empty_log_response() -> axum::response::Response {
    use axum::response::IntoResponse;
    axum::Json(serde_json::json!({
        "lines": [],
        "total_lines": 0,
        "next_since_line": 0,
        "truncated": false,
    }))
    .into_response()
}
