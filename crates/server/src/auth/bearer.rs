//! Bearer-token auth middleware for `/api/*`. Required because the
//! server bind can be widened off loopback (the historical
//! `0.0.0.0` default left an unauthenticated surface).
//!
//! The defaults are:
//!
//! * `127.0.0.1` bind + no `ECAA_SERVER_AUTH_TOKEN` set
//!   → `require == false` — every request passes through (local dev).
//! * Non-loopback bind OR `ECAA_SERVER_AUTH_TOKEN` set
//!   → `require == true` — every request must carry a matching
//!   `Authorization: Bearer <token>` header.
//!
//! The compare is constant-time via the `subtle` crate to keep the
//! shape (timing-safe token comparison) explicit and audit-friendly.

use axum::{
    body::Body,
    extract::State,
    http::{Method, Request, StatusCode},
    middleware::Next,
    response::Response,
};
use subtle::ConstantTimeEq;

/// Whether the shared-read-only-URL feature is on. Reads
/// `ECAA_SHARED_URLS_ENABLED` via the same `env_bool` helper that
/// `auth::principal::shared_urls_enabled` and `read_only::read_only_guard`
/// use, so the share-token gate stays consistent across the bearer bouncer
/// and `extract_principal`: tokens are honored iff the feature is enabled.
fn shared_urls_enabled() -> bool {
    ecaa_workflow_core::env_helpers::env_bool("ECAA_SHARED_URLS_ENABLED")
}

/// Whether the request carries a share-token, either as a `share_token=`
/// query parameter or an `X-Share-Token` header. Used only to decide
/// whether a SAFE-method request is eligible for the bearer-bouncer
/// exemption; the token is NOT validated here — `extract_principal`
/// fail-closes on an invalid token (403).
fn request_carries_share_token(req: &Request<Body>) -> bool {
    let has_query = req
        .uri()
        .query()
        .map(|q| q.split('&').any(|kv| kv.starts_with("share_token=")))
        .unwrap_or(false);
    has_query || req.headers().contains_key("X-Share-Token")
}

/// Bearer-token authentication configuration loaded from env-vars.
#[derive(Clone, Debug)]
pub struct AuthConfig {
    /// Expected bearer token; `None` when `ECAA_SERVER_AUTH_TOKEN` is unset.
    pub token: Option<String>,
    /// When true, every request must present a valid bearer token.
    pub require: bool,
}

impl AuthConfig {
    /// Build an `AuthConfig` from `ECAA_SERVER_AUTH_TOKEN` and the
    /// bind address. Requires auth when (a) a token is explicitly
    /// set, or (b) the bind address is non-loopback. The error case
    /// (non-loopback bind, no token) sets `require=true` with no
    /// token — every request will be rejected. That fail-closed
    /// behavior is intentional; the alternative is silently allowing
    /// LAN-exposed unauthenticated access.
    pub fn from_env(bind_addr: &str) -> Self {
        let token = std::env::var("ECAA_SERVER_AUTH_TOKEN")
            .ok()
            .filter(|t| !t.is_empty());
        let is_loopback = bind_addr.starts_with("127.0.0.1:") || bind_addr.starts_with("[::1]:");
        let require = token.is_some() || !is_loopback;
        if require && token.is_none() {
            tracing::error!(
                "server binds {bind_addr} (non-loopback) but ECAA_SERVER_AUTH_TOKEN is unset; \
                 all requests will be rejected. Set the env var or bind 127.0.0.1."
            );
        }
        Self { token, require }
    }
}

/// Axum middleware that enforces bearer-token auth when `cfg.require` is true.
pub async fn auth_middleware(
    State(cfg): State<AuthConfig>,
    req: Request<Body>,
    next: Next,
) -> Response {
    if !cfg.require {
        return next.run(req).await;
    }
    let Some(expected) = cfg.token.as_deref() else {
        return unauthorized();
    };
    // Share-link exemption (critical-analysis M5): a SAFE-method request
    // carrying a share-token (query param or X-Share-Token header) is let
    // past the bearer bouncer so `extract_principal` can resolve it to a
    // read-only ShareViewer. Bearer cannot validate the token here (no app
    // state); `extract_principal` fail-closes on an invalid token (403) and
    // `read_only_guard` rejects every mutation, so this only opens GET/HEAD
    // to share viewers — never a write path. A GET with no share-token still
    // falls through to the bearer check below and 401s.
    if shared_urls_enabled()
        && matches!(*req.method(), Method::GET | Method::HEAD)
        && request_carries_share_token(&req)
    {
        return next.run(req).await;
    }
    // After `strip_prefix("Bearer ")`, the remaining bytes are the
    // claimed token. Refuse outright when the token carries any
    // surrounding whitespace — `trim()` would silently normalize
    // "abc\n" to "abc" and pass authentication, which masks misuse
    // patterns (header injection, mistakenly newline-terminated
    // client tokens) instead of failing them loudly.
    let presented = req
        .headers()
        .get("Authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "))
        .filter(|s| !s.is_empty() && *s == s.trim());
    match presented {
        Some(got) if got.as_bytes().ct_eq(expected.as_bytes()).into() => next.run(req).await,
        _ => unauthorized(),
    }
}

fn unauthorized() -> Response {
    let mut resp = Response::new(Body::from(r#"{"error":"unauthorized"}"#));
    *resp.status_mut() = StatusCode::UNAUTHORIZED;
    resp.headers_mut()
        .insert("content-type", "application/json".parse().unwrap());
    resp.headers_mut().insert(
        "www-authenticate",
        "Bearer realm=\"ecaa-workflow\"".parse().unwrap(),
    );
    resp
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Serializes every test in this module that mutates the
    /// `ECAA_SERVER_AUTH_TOKEN` env var. `cargo test` runs tests within
    /// a binary in parallel; without this lock the three `AuthConfig`
    /// tests race each other and flake (one test reads the var while
    /// another has it transiently mutated).
    static AUTH_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn with_auth_env_lock<T>(body: impl FnOnce() -> T) -> T {
        let _guard = AUTH_ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        body()
    }

    #[test]
    fn loopback_bind_no_token_does_not_require_auth() {
        with_auth_env_lock(|| {
            // Saving env state to avoid cross-test pollution.
            let prior = std::env::var("ECAA_SERVER_AUTH_TOKEN").ok();
            std::env::remove_var("ECAA_SERVER_AUTH_TOKEN");
            let cfg = AuthConfig::from_env("127.0.0.1:3000");
            assert!(!cfg.require);
            assert!(cfg.token.is_none());
            if let Some(v) = prior {
                std::env::set_var("ECAA_SERVER_AUTH_TOKEN", v);
            }
        });
    }

    #[test]
    fn loopback_bind_with_token_requires_auth() {
        with_auth_env_lock(|| {
            let prior = std::env::var("ECAA_SERVER_AUTH_TOKEN").ok();
            std::env::set_var("ECAA_SERVER_AUTH_TOKEN", "abc");
            let cfg = AuthConfig::from_env("127.0.0.1:3000");
            assert!(cfg.require);
            assert_eq!(cfg.token.as_deref(), Some("abc"));
            match prior {
                Some(v) => std::env::set_var("ECAA_SERVER_AUTH_TOKEN", v),
                None => std::env::remove_var("ECAA_SERVER_AUTH_TOKEN"),
            }
        });
    }

    #[test]
    fn non_loopback_bind_requires_auth_even_without_token() {
        with_auth_env_lock(|| {
            let prior = std::env::var("ECAA_SERVER_AUTH_TOKEN").ok();
            std::env::remove_var("ECAA_SERVER_AUTH_TOKEN");
            let cfg = AuthConfig::from_env("0.0.0.0:3000");
            assert!(cfg.require);
            assert!(cfg.token.is_none());
            if let Some(v) = prior {
                std::env::set_var("ECAA_SERVER_AUTH_TOKEN", v);
            }
        });
    }
}
