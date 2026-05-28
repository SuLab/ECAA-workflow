//! Health endpoints. Liveness (`/healthz`) is always-200 + unauthenticated
//! — load balancers and uptime monitors carry no bearer token.
//! Readiness (`/readyz`) checks auth-config validity on non-loopback binds,
//! sessions-dir writability, and package-root writability, returning 503
//! with a structured failure list when not ready.

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use serde::Serialize;
use std::sync::Arc;

use crate::chat_routes::app_state::ChatAppState;

/// Liveness probe. Always returns 200 with the literal body `ok`.
/// No authentication required — load balancers and uptime monitors
/// need to probe this without a bearer token.
pub async fn healthz() -> &'static str {
    "ok"
}

#[derive(Serialize)]
struct ReadinessReport {
    ready: bool,
    failures: Vec<String>,
}

/// Readiness probe. Returns 200 when the server is configured correctly
/// and its writable directories are accessible; 503 with a structured
/// JSON failure list otherwise.
///
/// Checks performed:
/// 1. If the bind address is non-loopback and `server_auth_token` is absent,
///    report a configuration hazard (the auth middleware would reject every
///    request, making the server effectively unavailable).
/// 2. The sessions directory must be writable (can create a temp file).
/// 3. The package root must be writable (can create a temp file).
pub async fn readyz(State(state): State<Arc<ChatAppState>>) -> Response {
    let mut failures: Vec<String> = Vec::new();

    let cfg = &state.config;

    // Check 1: non-loopback bind without an auth token means the auth
    // middleware rejects every request — the server is running but
    // cannot serve any traffic. The bind_addr may be stored with or
    // without a port suffix ("127.0.0.1" or "127.0.0.1:3000"), so
    // check both forms.
    let bind_host = cfg
        .bind_addr
        .split(':')
        .next()
        .unwrap_or(&cfg.bind_addr)
        .trim_matches('[')
        .trim_matches(']');
    let is_loopback = bind_host == "127.0.0.1" || bind_host == "::1" || bind_host == "localhost";
    if !is_loopback && cfg.server_auth_token.is_none() {
        failures.push(format!(
            "non-loopback bind ({}) without SWFC_SERVER_AUTH_TOKEN — auth middleware will \
             reject every request",
            cfg.bind_addr
        ));
    }

    // Check 2: sessions directory is writable.
    if let Err(e) = probe_writable(&cfg.chat_sessions_dir) {
        failures.push(format!(
            "sessions dir {:?} is not writable: {}",
            cfg.chat_sessions_dir, e
        ));
    }

    // Check 3: package root is writable.
    if let Err(e) = probe_writable(&cfg.package_root) {
        failures.push(format!(
            "package root {:?} is not writable: {}",
            cfg.package_root, e
        ));
    }

    let ready = failures.is_empty();
    let body = ReadinessReport { ready, failures };
    let status = if ready {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    (status, Json(body)).into_response()
}

/// Try to create and immediately remove a temp file inside `dir` to
/// verify write access. Creates the directory first (best-effort) so a
/// freshly-provisioned host whose sessions/package dirs haven't been
/// seeded yet doesn't fail readiness before the first session.
fn probe_writable(dir: &std::path::Path) -> Result<(), String> {
    // Attempt directory creation; ignore the error if it already exists.
    if let Err(e) = std::fs::create_dir_all(dir) {
        return Err(format!("cannot create directory: {}", e));
    }
    let probe = dir.join(".readyz-probe");
    std::fs::write(&probe, b"").map_err(|e| format!("write failed: {}", e))?;
    let _ = std::fs::remove_file(&probe);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_writable_succeeds_on_tmp() {
        let dir = tempfile::tempdir().unwrap();
        assert!(probe_writable(dir.path()).is_ok());
    }

    #[test]
    fn probe_writable_fails_on_nonexistent_read_only_path() {
        // `/proc/1/fd` is an always-present unwritable path on Linux.
        // Skip if we happen to be running as root (probe would pass).
        if std::env::var("SWFC_TEST_SKIP_READYZ_UNWRITABLE").is_ok() {
            return;
        }
        // Use a path under / that we can't write to.
        let result = probe_writable(std::path::Path::new("/proc/1/fd/unwritable-test-path"));
        // Either create_dir_all or write should fail.
        assert!(
            result.is_err(),
            "expected write to fail on an unwritable path"
        );
    }
}
