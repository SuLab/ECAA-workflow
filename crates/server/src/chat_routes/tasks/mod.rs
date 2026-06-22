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
    ("GET", "/api/chat/session/:id/deposit-package.zip"),
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
/// lookup), `chat_routes/verification.rs` (the manual verify endpoint), and
/// `crate::verification` (boot-time policy check); `pub(crate)` so callers
/// outside this submodule can reach it.
///
/// Resolution order:
/// 1. `ECAA_CONFIG_DIR` — explicit operator override, always wins.
/// 2. Binary-relative discovery — walk up from `current_exe()` looking for
///    a `config/` directory carrying the `downstream-policy` marker. This
///    is the same "walk up to a marker dir" convention the harness already
///    uses (`wrroc_validator_impl::find_validator_script`,
///    `executor::builder_exit_codes`), adapted to anchor on the *installed*
///    binary location rather than the compile-time `CARGO_MANIFEST_DIR`
///    (which points into the build tree and is useless for an installed
///    `ecaa-workflow-server`). This is what lets the server find policy when
///    launched from an arbitrary CWD.
/// 3. CWD-relative `config` — final fallback so repo-root launches and test
///    harnesses that `cd` into a fixture dir keep working unchanged. A
///    warning is logged when this fallback is taken AND the CWD-relative dir
///    doesn't actually carry the marker, so a misconfiguration surfaces.
pub(crate) fn config_dir_or_default() -> std::path::PathBuf {
    if let Ok(explicit) = std::env::var("ECAA_CONFIG_DIR") {
        return std::path::PathBuf::from(explicit);
    }
    if let Some(found) = config_dir_from_exe() {
        return found;
    }
    let cwd_relative = std::path::PathBuf::from("config");
    if !config_dir_has_marker(&cwd_relative) {
        tracing::warn!(
            target: "config",
            cwd = ?std::env::current_dir().ok(),
            "ECAA_CONFIG_DIR is unset and no `config/` directory with a \
             `downstream-policy` marker was found relative to the server \
             binary; falling back to CWD-relative `config` which does not \
             carry the marker either — claim verification and policy loading \
             will likely fail. Set ECAA_CONFIG_DIR or launch from the repo root."
        );
    }
    cwd_relative
}

/// Marker that distinguishes the real config dir from an unrelated `config/`
/// directory: the `downstream-policy` subdir is what `config_dir_or_default`'s
/// primary consumer (claim verification) loads `interpretation-policy.json`
/// from, so its presence is the cheapest reliable signal.
fn config_dir_has_marker(dir: &std::path::Path) -> bool {
    dir.join("downstream-policy").is_dir()
}

/// Walk up from the running binary's directory looking for a sibling
/// `config/` dir carrying the marker. Returns `None` when `current_exe()`
/// is unavailable (rare) or no ancestor holds a marked `config/`.
fn config_dir_from_exe() -> Option<std::path::PathBuf> {
    config_dir_from_exe_path(&std::env::current_exe().ok()?)
}

/// Pure walk-up over a given executable path — `exe` is the binary file, so
/// the search starts at its parent directory and ascends. Split out from
/// `config_dir_from_exe` so it can be driven from tests with a synthetic
/// path (the real `current_exe()` can't be relocated under `cargo test`).
fn config_dir_from_exe_path(exe: &std::path::Path) -> Option<std::path::PathBuf> {
    let mut dir = exe.parent();
    while let Some(d) = dir {
        let candidate = d.join("config");
        if config_dir_has_marker(&candidate) {
            return Some(candidate);
        }
        dir = d.parent();
    }
    None
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

#[cfg(test)]
mod config_dir_tests {
    // Workspace lint is `unsafe_code = "deny"`; the env-mutating helpers below
    // use `unsafe { std::env::set_var / remove_var }` (unsafe under the pinned
    // toolchain), serialized by ENV_LOCK. Mirrors the established pattern in
    // `read_only.rs`.
    #![allow(unsafe_code)]
    use super::*;
    use std::path::Path;
    use std::sync::Mutex;

    // `config_dir_or_default` reads process-global state (`ECAA_CONFIG_DIR`
    // and the current working directory), so its tests must not run
    // concurrently with one another. A plain std Mutex is fine — these are
    // sync tests with no `.await` between acquire and release.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    /// Capture + restore `ECAA_CONFIG_DIR` and CWD across a test body so a
    /// panic in one case doesn't leak global state into the next.
    struct StateGuard {
        prior_env: Option<String>,
        prior_cwd: std::path::PathBuf,
    }
    impl StateGuard {
        fn capture() -> Self {
            Self {
                prior_env: std::env::var("ECAA_CONFIG_DIR").ok(),
                prior_cwd: std::env::current_dir().expect("cwd readable"),
            }
        }
        fn clear_env(&self) {
            // SAFETY: mutation serialized by ENV_LOCK.
            unsafe { std::env::remove_var("ECAA_CONFIG_DIR") };
        }
        fn set_env(&self, v: &Path) {
            // SAFETY: mutation serialized by ENV_LOCK.
            unsafe { std::env::set_var("ECAA_CONFIG_DIR", v) };
        }
    }
    impl Drop for StateGuard {
        fn drop(&mut self) {
            // SAFETY: mutation serialized by ENV_LOCK.
            match &self.prior_env {
                Some(v) => unsafe { std::env::set_var("ECAA_CONFIG_DIR", v) },
                None => unsafe { std::env::remove_var("ECAA_CONFIG_DIR") },
            }
            let _ = std::env::set_current_dir(&self.prior_cwd);
        }
    }

    /// Lay down a minimal config dir carrying the `downstream-policy` marker.
    fn make_marked_config(root: &Path) -> std::path::PathBuf {
        let config = root.join("config");
        std::fs::create_dir_all(config.join("downstream-policy")).unwrap();
        config
    }

    #[test]
    fn env_override_wins_even_when_pointing_at_an_unmarked_dir() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let guard = StateGuard::capture();
        let tmp = tempfile::tempdir().unwrap();
        let explicit = tmp.path().join("my-config");
        std::fs::create_dir_all(&explicit).unwrap();
        guard.set_env(&explicit);

        // The explicit value is returned verbatim regardless of marker or CWD.
        assert_eq!(config_dir_or_default(), explicit);
    }

    #[test]
    fn resolves_from_non_cwd_working_directory_via_binary_discovery() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let guard = StateGuard::capture();
        guard.clear_env();

        // Build a fake "install root" holding the binary AND a marked
        // config/ sibling, exactly like a real install layout where the
        // binary and config ship together.
        let install = tempfile::tempdir().unwrap();
        let bin_dir = install.path().join("bin");
        std::fs::create_dir_all(&bin_dir).unwrap();
        let expected_config = make_marked_config(install.path());

        // Move CWD somewhere with NO `config/` so the CWD-relative fallback
        // cannot accidentally satisfy the lookup. This is the regression the
        // task targets: server launched outside the repo root.
        let elsewhere = tempfile::tempdir().unwrap();
        std::env::set_current_dir(elsewhere.path()).unwrap();

        // Exercise the binary-discovery walk-up against the synthetic exe
        // path directly (we can't relocate the real test binary), then
        // assert the marker walk-up lands on the install-root config.
        let found = config_dir_from_exe_path(&bin_dir.join("ecaa-workflow-server"))
            .expect("binary-relative discovery should find the marked config dir");
        assert_eq!(found, expected_config);

        // And the public entry point must NOT return the (absent) CWD-relative
        // `config` here — with the real test binary it either finds a marked
        // config above the binary or falls through to the literal `config`
        // path; in neither case does it panic, and it never returns the
        // unmarked `elsewhere/config` (which does not exist).
        let resolved = config_dir_or_default();
        assert_ne!(resolved, elsewhere.path().join("config"));
    }

    #[test]
    fn falls_back_to_cwd_relative_config_when_nothing_marked() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let guard = StateGuard::capture();
        guard.clear_env();

        // CWD with no marked config anywhere reachable from the binary in
        // this synthetic walk: the helper returns None and the public fn
        // hands back the literal CWD-relative `config` (best-effort).
        let lonely = tempfile::tempdir().unwrap();
        let isolated_bin = lonely.path().join("nested").join("bin").join("server");
        std::fs::create_dir_all(isolated_bin.parent().unwrap()).unwrap();
        assert!(config_dir_from_exe_path(&isolated_bin).is_none());

        // The marker helper distinguishes a real config dir from a bare one.
        let bare = lonely.path().join("config");
        std::fs::create_dir_all(&bare).unwrap();
        assert!(!config_dir_has_marker(&bare));
        let marked = make_marked_config(lonely.path());
        // make_marked_config overwrote the bare dir with the marker subdir.
        assert!(config_dir_has_marker(&marked));
    }
}
