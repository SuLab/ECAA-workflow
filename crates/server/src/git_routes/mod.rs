//! Git-backed provenance for emitted packages, amendments, and branched
//! sessions.
//!
//! Per-package shape:
//! - Top-level routes (`/api/git/config`, `/api/git/keys/ssh`,
//!   `/api/git/test-connection`) handle admin-style state — config CRUD
//! + SSH key generation + remote-reachability dry-runs.
//! - Per-session routes (`/api/git/session/:id/{init,status,log,commit,
//! push,remote}`) operate on the session's emitted package directory.
//!   Each route looks up `session.emitted_package_path` and refuses
//!   (404) when the session has no emitted package.
//!
//! Design rules:
//! - No `libgit2` / `git2` / `gix` dep — shell out to the system `git`
//!   binary via `std::process::Command`. Keeps the sync invariant,
//!   avoids FFI build pain, and lets users keep their own
//!   `~/.gitconfig` + ssh-agent.
//! - Off by default. `GitConfig.enabled = false` is the initial state;
//!   `ECAA_GIT_ENABLED=0` is a kill switch that overrides the config
//!   regardless of the UI checkbox.
//! - Private SSH keys never cross the HTTP boundary. The config stores
//!   a filesystem path; the UI only ever receives the public key +
//!   that path.
//! - Every route runs synchronously on an Axum blocking-task handle
//!   (git shell-out can take seconds). The conversation service stays
//!   async; the commit hooks are fired from tokio's
//!   `spawn_blocking` wrapper.

mod config;
pub mod service;

pub use config::{git_config_path, GitConfig};
pub use service::{CommitInput, GitService};

/// Shared serializer for tests that mutate `HOME` / `ECAA_GIT_ENABLED`.
/// `std::env::set_var` is process-global, so concurrent test threads
/// racing on the same key produce flaky results. Both `config.rs` and
/// `service.rs` use this lock so cross-module test parallelism is safe.
#[cfg(test)]
pub(crate) static GIT_TEST_ENV_LOCK: parking_lot::Mutex<()> = parking_lot::Mutex::new(());

use axum::{
    extract::{Path as AxumPath, Query, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use parking_lot::{RwLock, RwLockReadGuard};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;
use uuid::Uuid;

use crate::chat_routes::ChatAppState;

/// Query parameters for `GET /api/git/session/:id/log`.
#[derive(Debug, Clone, Deserialize)]
pub struct LogQuery {
    /// Maximum number of log entries to return; defaults to 20, clamped to 500.
    #[serde(default = "default_log_limit")]
    pub limit: usize,
}

fn default_log_limit() -> usize {
    20
}

/// Response from `GET /api/git/session/:id/status`.
#[derive(Debug, Clone, Serialize)]
pub struct GitStatusResponse {
    /// Path to the session's emitted package — echoed back so the UI
    /// can confirm which package the call inspected.
    pub repo_path: String,
    /// Remote URL from config, when set.
    pub remote_url: Option<String>,
    /// Whether the git binary is on PATH (`true`) — absent means the
    /// routes will return 503 until git is installed.
    pub git_available: bool,
    /// Whether `<package_dir>/.git` exists. When false, `POST.../init`
    /// should be offered to the UI.
    pub initialized: bool,
    /// Last commit's short sha + subject, or null when the repo has no
    /// commits yet.
    pub last_commit: Option<CommitSummary>,
    /// Dirty file count (output of `git status --porcelain | wc -l`).
    pub dirty_count: u32,
    /// Number of commits on the current branch.
    pub commit_count: u64,
}

/// Short summary of a git commit, included in `GitStatusResponse`.
#[derive(Debug, Clone, Serialize)]
pub struct CommitSummary {
    /// Short (7-char) commit SHA.
    pub sha: String,
    /// Commit subject line.
    pub subject: String,
    /// Unix seconds (committer date).
    pub committed_at: u64,
}

/// Request body for `POST /api/git/keys/ssh`.
#[derive(Debug, Clone, Deserialize)]
pub struct GenerateSshKeyRequest {
    /// Target path under the user's home dir. Validated against the
    /// user's actual $HOME; paths outside $HOME are rejected.
    pub path: String,
    /// Optional comment; defaults to "scripps-workflow-<hostname>".
    #[serde(default)]
    pub comment: Option<String>,
}

/// Response from `POST /api/git/keys/ssh`.
#[derive(Debug, Clone, Serialize)]
pub struct GenerateSshKeyResponse {
    /// Absolute path to the newly-written private key (the UI never
    /// reads this file, just echoes the path).
    pub private_key_path: String,
    /// Full contents of the `<path>.pub` file so the UI can render it
    /// for the user to paste into GitHub/GitLab Deploy Keys.
    pub public_key: String,
}

/// Request body for `POST /api/git/test-connection`.
#[derive(Debug, Clone, Deserialize)]
pub struct TestConnectionRequest {
    /// Override the configured remote URL for a one-shot dry-run.
    /// Absent = use `GitConfig.remote_url`.
    #[serde(default)]
    pub remote_url: Option<String>,
}

/// Response from `POST /api/git/test-connection`.
#[derive(Debug, Clone, Serialize)]
pub struct TestConnectionResponse {
    /// True when `git ls-remote` exited zero.
    pub reachable: bool,
    /// Raw stderr from `git ls-remote` when `reachable=false` so the UI
    /// can surface the specific auth / host error.
    pub error: Option<String>,
}

/// Request body for `POST /api/git/session/:id/commit`.
#[derive(Debug, Clone, Deserialize)]
pub struct CommitRequest {
    /// Commit subject; the service prepends no additional prefix.
    pub message: String,
    /// When present, `git add <paths>` before `commit`. Empty =
    /// `git add -A`.
    #[serde(default)]
    pub paths: Vec<String>,
    /// Push after commit when true, regardless of
    /// `GitConfig.auto_push`.
    #[serde(default)]
    pub push: bool,
}

/// Response from `POST /api/git/session/:id/commit`.
#[derive(Debug, Clone, Serialize)]
pub struct CommitResponse {
    /// Full SHA of the new commit.
    pub sha: String,
    /// Whether the commit was also pushed to the remote.
    pub pushed: bool,
}

/// Request body for `POST /api/git/session/:id/remote`.
#[derive(Debug, Clone, Deserialize)]
pub struct SetRemoteRequest {
    /// Absent = clear the remote. Present + non-empty = `git remote
    /// add origin <url>` (or `set-url` when origin already exists).
    #[serde(default)]
    pub remote_url: Option<String>,
}

// ── Helpers ─────────────────────────────────────────────────────────────────

/// Look up the emitted package directory for a session. Returns 404
/// when the session doesn't exist or hasn't emitted a package yet.
async fn resolve_package_dir(
    app: &ChatAppState,
    session_id: Uuid,
) -> Result<PathBuf, (StatusCode, Json<serde_json::Value>)> {
    let Some(s) = app.conversation.get_session(session_id).await else {
        return Err((
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": "session not found"})),
        ));
    };
    let Some(pkg) = s.emitted_package_path.clone() else {
        return Err((
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": "session has no emitted package"})),
        ));
    };
    Ok(pkg)
}

/// Common wrapper: resolve the package dir on the async runtime, then
/// run a blocking `git` call inside `spawn_blocking`. Each handler
/// passes a closure that takes the resolved path and returns either
/// the response payload (Ok) or a string error (BAD_REQUEST).
async fn with_session_package_dir<F, T>(
    app: ChatAppState,
    session_id: Uuid,
    f: F,
) -> axum::response::Response
where
    F: FnOnce(PathBuf) -> anyhow::Result<T> + Send + 'static,
    T: serde::Serialize + Send + 'static,
{
    let pkg = match resolve_package_dir(&app, session_id).await {
        Ok(p) => p,
        Err((status, body)) => return (status, body).into_response(),
    };
    match tokio::task::spawn_blocking(move || f(pkg)).await {
        Ok(Ok(value)) => Json(value).into_response(),
        Ok(Err(e)) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": format!("{}", e)})),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": format!("join: {}", e)})),
        )
            .into_response(),
    }
}

// ── Route handlers ──────────────────────────────────────────────────────────

/// Strip userinfo (username + password)
/// from an https-style remote URL before serving it to the UI / any
/// client that reads `/api/git/config`. An HTTPS remote like
/// `https://user:tok@github.com/o/r.git` is rewritten to
/// `https://github.com/o/r.git`.
///
/// Pass-through cases:
/// - ssh-style URLs (`git@github.com:o/r.git`) aren't valid `url::Url`
///   inputs; we return them unchanged since they don't carry inline
///   passwords.
/// - URLs that already lack userinfo round-trip cleanly.
///
/// Used by `get_config` to avoid disclosing tokens to anyone who can
/// hit the API surface (LAN attacker on a wide-bound server, or any
/// session-share-token holder).
fn redact_remote_url(url: &str) -> String {
    match url::Url::parse(url) {
        Ok(mut u) => {
            let _ = u.set_password(None);
            let _ = u.set_username("");
            u.to_string()
        }
        // ssh-style URLs (`git@host:path`) and other non-URL strings
        // are passed through — they don't carry inline credentials.
        Err(_) => url.to_string(),
    }
}

/// `GET /api/git/config` — return the current `GitConfig` with userinfo redacted.
pub async fn get_config(State(app): State<ChatAppState>) -> impl IntoResponse {
    let mut cfg = app.git_config().read().clone();
    // Never echo userinfo back to the UI / API consumers.
    if let Some(url) = cfg.remote_url.as_deref() {
        let redacted = redact_remote_url(url);
        cfg.remote_url = Some(redacted);
    }
    Json(cfg).into_response()
}

/// `PUT /api/git/config` — validate and persist a new `GitConfig`.
pub async fn put_config(
    State(app): State<ChatAppState>,
    Json(cfg): Json<GitConfig>,
) -> impl IntoResponse {
    match app.git_config().update(cfg.clone()) {
        Ok(()) => Json(cfg).into_response(),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": format!("{}", e)})),
        )
            .into_response(),
    }
}

/// `POST /api/git/keys/ssh` — generate a new SSH key pair at the requested path.
pub async fn post_generate_ssh_key(Json(req): Json<GenerateSshKeyRequest>) -> impl IntoResponse {
    match tokio::task::spawn_blocking(move || service::generate_ssh_key(&req)).await {
        Ok(Ok(resp)) => Json(resp).into_response(),
        Ok(Err(e)) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": format!("{}", e)})),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": format!("join: {}", e)})),
        )
            .into_response(),
    }
}

/// `POST /api/git/test-connection` — dry-run `git ls-remote` against the configured remote.
pub async fn post_test_connection(
    State(app): State<ChatAppState>,
    Json(req): Json<TestConnectionRequest>,
) -> impl IntoResponse {
    let mut cfg = app.git_config().read().clone();
    if let Some(override_url) = req.remote_url {
        cfg.remote_url = Some(override_url);
    }
    // Per-package shape: test_remote doesn't need a working tree (it
    // shells out `git ls-remote` against the configured URL). The
    // service's `run_git` invocation runs `git -C <path>`, but `git
    // ls-remote` works even when the path isn't a repo. Pass
    // `std::env::temp_dir()` (always writable, always exists) so the
    // service constructs without touching any package directory.
    let svc = GitService::for_package(&cfg, &std::env::temp_dir());
    match tokio::task::spawn_blocking(move || svc.test_remote()).await {
        Ok(Ok(())) => Json(TestConnectionResponse {
            reachable: true,
            error: None,
        })
        .into_response(),
        Ok(Err(e)) => Json(TestConnectionResponse {
            reachable: false,
            error: Some(format!("{}", e)),
        })
        .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": format!("join: {}", e)})),
        )
            .into_response(),
    }
}

/// `POST /api/git/session/:id/init` — `git init` the session's
/// package directory + apply user.name/email + optional remote.
/// Idempotent on already-initialized repos.
pub async fn post_session_init(
    State(app): State<ChatAppState>,
    AxumPath(session_id): AxumPath<Uuid>,
) -> impl IntoResponse {
    let pkg = match resolve_package_dir(&app, session_id).await {
        Ok(p) => p,
        Err((status, body)) => return (status, body).into_response(),
    };
    let cfg = app.git_config().read().clone();
    match tokio::task::spawn_blocking(move || GitService::for_package(&cfg, &pkg).init()).await {
        Ok(Ok(())) => StatusCode::NO_CONTENT.into_response(),
        Ok(Err(e)) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": format!("{}", e)})),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": format!("join: {}", e)})),
        )
            .into_response(),
    }
}

/// `GET /api/git/session/:id/status` — read the per-package
/// `.git` state.
pub async fn get_session_status(
    State(app): State<ChatAppState>,
    AxumPath(session_id): AxumPath<Uuid>,
) -> impl IntoResponse {
    let pkg = match resolve_package_dir(&app, session_id).await {
        Ok(p) => p,
        Err((status, body)) => return (status, body).into_response(),
    };
    let cfg = app.git_config().read().clone();
    let repo_path = pkg.to_string_lossy().to_string();
    // Redact userinfo from the remote_url field before
    // surfacing the status to the UI / any HTTP consumer.
    let remote_url = cfg.remote_url.as_deref().map(redact_remote_url);
    let pkg_for_call = pkg.clone();
    let cfg_for_call = cfg.clone();
    match tokio::task::spawn_blocking(move || {
        GitService::for_package(&cfg_for_call, &pkg_for_call).status()
    })
    .await
    {
        Ok(Ok((initialized, last_commit, dirty_count, commit_count))) => {
            let resp = GitStatusResponse {
                repo_path,
                remote_url,
                git_available: service::git_on_path(),
                initialized,
                last_commit,
                dirty_count,
                commit_count,
            };
            Json(resp).into_response()
        }
        Ok(Err(e)) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": format!("{}", e)})),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": format!("join: {}", e)})),
        )
            .into_response(),
    }
}

/// `GET /api/git/session/:id/log` — return the most recent commits from the package repo.
pub async fn get_session_log(
    State(app): State<ChatAppState>,
    AxumPath(session_id): AxumPath<Uuid>,
    Query(q): Query<LogQuery>,
) -> impl IntoResponse {
    let limit = q.limit.clamp(1, 500);
    let cfg = app.git_config().read().clone();
    with_session_package_dir(app, session_id, move |pkg| {
        GitService::for_package(&cfg, &pkg).log(limit)
    })
    .await
}

/// `POST /api/git/session/:id/commit` — stage + commit + optionally push the package repo.
pub async fn post_session_commit(
    State(app): State<ChatAppState>,
    AxumPath(session_id): AxumPath<Uuid>,
    Json(req): Json<CommitRequest>,
) -> impl IntoResponse {
    let cfg = app.git_config().read().clone();
    if !cfg.effective_enabled() {
        return (
            StatusCode::CONFLICT,
            Json(serde_json::json!({"error": "git integration disabled"})),
        )
            .into_response();
    }
    let pkg = match resolve_package_dir(&app, session_id).await {
        Ok(p) => p,
        Err((status, body)) => return (status, body).into_response(),
    };
    let push = req.push || cfg.auto_push;
    let input = CommitInput {
        subject: req.message,
        paths: req.paths,
    };
    let cfg_for_call = cfg.clone();
    match tokio::task::spawn_blocking(move || {
        let svc = GitService::for_package(&cfg_for_call, &pkg);
        // Auto-init: callers expect commit-then-push to "just work"
        // on a fresh package. Idempotent for already-initialized repos.
        svc.init()?;
        svc.commit_and_maybe_push(&input, push)
    })
    .await
    {
        Ok(Ok((sha, pushed))) => Json(CommitResponse { sha, pushed }).into_response(),
        Ok(Err(e)) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": format!("{}", e)})),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": format!("join: {}", e)})),
        )
            .into_response(),
    }
}

/// `POST /api/git/session/:id/push` — push the package repo to the configured remote.
pub async fn post_session_push(
    State(app): State<ChatAppState>,
    AxumPath(session_id): AxumPath<Uuid>,
) -> impl IntoResponse {
    let cfg = app.git_config().read().clone();
    if !cfg.effective_enabled() {
        return (
            StatusCode::CONFLICT,
            Json(serde_json::json!({"error": "git integration disabled"})),
        )
            .into_response();
    }
    let pkg = match resolve_package_dir(&app, session_id).await {
        Ok(p) => p,
        Err((status, body)) => return (status, body).into_response(),
    };
    match tokio::task::spawn_blocking(move || GitService::for_package(&cfg, &pkg).push()).await {
        Ok(Ok(())) => StatusCode::NO_CONTENT.into_response(),
        Ok(Err(e)) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": format!("{}", e)})),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": format!("join: {}", e)})),
        )
            .into_response(),
    }
}

/// `POST /api/git/session/:id/remote` — set or clear the `origin`
/// remote on the per-package repo. Absent / empty `remote_url` clears
/// the remote; non-empty `remote_url` sets it.
pub async fn post_session_remote(
    State(app): State<ChatAppState>,
    AxumPath(session_id): AxumPath<Uuid>,
    Json(req): Json<SetRemoteRequest>,
) -> impl IntoResponse {
    let pkg = match resolve_package_dir(&app, session_id).await {
        Ok(p) => p,
        Err((status, body)) => return (status, body).into_response(),
    };
    let cfg = app.git_config().read().clone();
    match tokio::task::spawn_blocking(move || {
        let svc = GitService::for_package(&cfg, &pkg);
        // Auto-init so the SME can set a remote on a freshly-emitted
        // package without a separate /init call.
        svc.init()?;
        match req
            .remote_url
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            Some(url) => svc.set_remote(url),
            None => svc.clear_remote(),
        }
    })
    .await
    {
        Ok(Ok(())) => StatusCode::NO_CONTENT.into_response(),
        Ok(Err(e)) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": format!("{}", e)})),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": format!("join: {}", e)})),
        )
            .into_response(),
    }
}

// ── State helpers ───────────────────────────────────────────────────────────

/// Live-reload wrapper around [`GitConfig`]. Owned by `ChatAppState`,
/// accessed by handlers. Holds a `RwLock<GitConfig>` + the config
/// path; writes go through `update()` which atomically renames a
/// `.tmp` file.
pub struct GitConfigStore {
    path: std::path::PathBuf,
    inner: RwLock<GitConfig>,
}

impl GitConfigStore {
    /// Load from `path` (default config on missing / parse failure).
    pub fn open_or_default(path: std::path::PathBuf) -> Self {
        let inner = GitConfig::load_or_default(&path);
        Self {
            path,
            inner: RwLock::new(inner),
        }
    }

    /// Acquire a read lock and return a guard over the current `GitConfig`.
    pub fn read(&self) -> RwLockReadGuard<'_, GitConfig> {
        self.inner.read()
    }

    /// Validate + atomically persist a new config. Path validation
    /// (ssh_key_path must resolve inside $HOME when set) runs before
    /// the write so a bad update doesn't stomp the good state on disk.
    ///
    /// `parking_lot::RwLock` does not poison on writer panic, so a
    /// panicking caller leaves the inner GitConfig in its prior state
    /// rather than blowing up subsequent readers — matches the
    /// "in-memory state is the source of truth" doctrine.
    pub fn update(&self, next: GitConfig) -> anyhow::Result<()> {
        next.validate()?;
        next.save(&self.path)?;
        *self.inner.write() = next;
        Ok(())
    }
}

/// Arc-shared handle returned from `ChatAppState`. `parking_lot::RwLock`
/// is used over `std::sync::RwLock` so read-only handlers don't pay a
/// guard cost in the happy path.
pub type GitConfigHandle = Arc<GitConfigStore>;

/// Mount git routes onto a new Router. `merge`d into the main chat
/// router in `main.rs`. Per-package shape: admin routes (`config`,
/// `keys/ssh`, `test-connection`) at the top level; package-scoped
/// routes (`init`, `status`, `log`, `commit`, `push`, `remote`) under
/// `/api/git/session/:id/`.
pub fn router(app: ChatAppState) -> axum::Router {
    axum::Router::new()
        .route(
            "/api/git/config",
            axum::routing::get(get_config).put(put_config),
        )
        .route(
            "/api/git/keys/ssh",
            axum::routing::post(post_generate_ssh_key),
        )
        .route(
            "/api/git/test-connection",
            axum::routing::post(post_test_connection),
        )
        .route(
            "/api/git/session/:id/init",
            axum::routing::post(post_session_init),
        )
        .route(
            "/api/git/session/:id/status",
            axum::routing::get(get_session_status),
        )
        .route(
            "/api/git/session/:id/log",
            axum::routing::get(get_session_log),
        )
        .route(
            "/api/git/session/:id/commit",
            axum::routing::post(post_session_commit),
        )
        .route(
            "/api/git/session/:id/push",
            axum::routing::post(post_session_push),
        )
        .route(
            "/api/git/session/:id/remote",
            axum::routing::post(post_session_remote),
        )
        .with_state(app)
}

#[cfg(test)]
mod redact_remote_tests {
    //! `redact_remote_url` correctness
    //! gate. The `get_config` handler depends on this function to
    //! prevent userinfo (`https://user:tok@…`) from leaking to the
    //! UI / any consumer of `GET /api/git/config`.

    use super::*;

    #[test]
    fn strips_userinfo_from_https_url() {
        let cleaned = redact_remote_url("https://user:tok@github.com/o/r.git");
        assert_eq!(cleaned, "https://github.com/o/r.git");
    }

    #[test]
    fn strips_user_only_when_password_absent() {
        // `https://user@host/...` is still a userinfo leak — the
        // username alone can identify the account.
        let cleaned = redact_remote_url("https://user@github.com/o/r.git");
        assert_eq!(cleaned, "https://github.com/o/r.git");
    }

    #[test]
    fn passes_through_ssh_url() {
        // ssh-style URLs aren't a valid `url::Url` — pass through.
        let cleaned = redact_remote_url("git@github.com:o/r.git");
        assert_eq!(cleaned, "git@github.com:o/r.git");
    }

    #[test]
    fn passes_through_https_without_userinfo() {
        let cleaned = redact_remote_url("https://github.com/o/r.git");
        assert_eq!(cleaned, "https://github.com/o/r.git");
    }

    #[test]
    fn handles_ssh_protocol_scheme() {
        // `ssh://user@host:port/path` IS a valid Url; usernames there
        // are conventional (e.g. `git`) and `set_username("")` strips
        // them. Even when the username is just `git`, removing it is
        // safe because the protocol re-attaches it from
        // `~/.ssh/config` at connection time.
        let cleaned = redact_remote_url("ssh://git@github.com:22/o/r.git");
        // The exact normalized form depends on `url` — assert it has
        // no `git@` prefix.
        assert!(
            !cleaned.contains("git@"),
            "ssh:// userinfo retained: {cleaned}"
        );
    }

    #[test]
    fn empty_url_passes_through() {
        // Pathological input from a never-set remote_url — the parser
        // returns Err, so we pass through.
        let cleaned = redact_remote_url("");
        assert_eq!(cleaned, "");
    }

    #[test]
    fn malformed_url_passes_through() {
        // Garbage that isn't a URL is returned untouched (better than
        // 500ing in the handler).
        let cleaned = redact_remote_url("not a url at all");
        assert_eq!(cleaned, "not a url at all");
    }
}
