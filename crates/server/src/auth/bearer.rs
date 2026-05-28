//! Bearer-token auth middleware for `/api/*`. Required because the
//! server bind can be widened off loopback (the historical
//! `0.0.0.0` default left an unauthenticated surface).
//!
//! The defaults are:
//!
//! * `127.0.0.1` bind + no `SWFC_SERVER_AUTH_TOKEN` set
//!   → `require == false` — every request passes through (local dev).
//! * Non-loopback bind OR `SWFC_SERVER_AUTH_TOKEN` set
//!   → `require == true` — every request must carry a matching
//!   `Authorization: Bearer <token>` header.
//!
//! The compare is constant-time via the `subtle` crate to keep the
//! shape (timing-safe token comparison) explicit and audit-friendly.

use axum::{
    body::Body,
    extract::State,
    http::{Request, StatusCode},
    middleware::Next,
    response::Response,
};
use subtle::ConstantTimeEq;

/// Bearer-token authentication configuration loaded from env-vars.
#[derive(Clone, Debug)]
pub struct AuthConfig {
    /// Expected bearer token; `None` when `SWFC_SERVER_AUTH_TOKEN` is unset.
    pub token: Option<String>,
    /// When true, every request must present a valid bearer token.
    pub require: bool,
}

impl AuthConfig {
    /// Build an `AuthConfig` from `SWFC_SERVER_AUTH_TOKEN` and the
    /// bind address. Requires auth when (a) a token is explicitly
    /// set, or (b) the bind address is non-loopback. The error case
    /// (non-loopback bind, no token) sets `require=true` with no
    /// token — every request will be rejected. That fail-closed
    /// behavior is intentional; the alternative is silently allowing
    /// LAN-exposed unauthenticated access.
    pub fn from_env(bind_addr: &str) -> Self {
        let token = std::env::var("SWFC_SERVER_AUTH_TOKEN")
            .ok()
            .filter(|t| !t.is_empty());
        let is_loopback = bind_addr.starts_with("127.0.0.1:") || bind_addr.starts_with("[::1]:");
        let require = token.is_some() || !is_loopback;
        if require && token.is_none() {
            tracing::error!(
                "server binds {bind_addr} (non-loopback) but SWFC_SERVER_AUTH_TOKEN is unset; \
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
        "Bearer realm=\"scripps-workflow\"".parse().unwrap(),
    );
    resp
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Serializes every test in this module that mutates the
    /// `SWFC_SERVER_AUTH_TOKEN` env var. `cargo test` runs tests within
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
            let prior = std::env::var("SWFC_SERVER_AUTH_TOKEN").ok();
            std::env::remove_var("SWFC_SERVER_AUTH_TOKEN");
            let cfg = AuthConfig::from_env("127.0.0.1:3000");
            assert!(!cfg.require);
            assert!(cfg.token.is_none());
            if let Some(v) = prior {
                std::env::set_var("SWFC_SERVER_AUTH_TOKEN", v);
            }
        });
    }

    #[test]
    fn loopback_bind_with_token_requires_auth() {
        with_auth_env_lock(|| {
            let prior = std::env::var("SWFC_SERVER_AUTH_TOKEN").ok();
            std::env::set_var("SWFC_SERVER_AUTH_TOKEN", "abc");
            let cfg = AuthConfig::from_env("127.0.0.1:3000");
            assert!(cfg.require);
            assert_eq!(cfg.token.as_deref(), Some("abc"));
            match prior {
                Some(v) => std::env::set_var("SWFC_SERVER_AUTH_TOKEN", v),
                None => std::env::remove_var("SWFC_SERVER_AUTH_TOKEN"),
            }
        });
    }

    #[test]
    fn non_loopback_bind_requires_auth_even_without_token() {
        with_auth_env_lock(|| {
            let prior = std::env::var("SWFC_SERVER_AUTH_TOKEN").ok();
            std::env::remove_var("SWFC_SERVER_AUTH_TOKEN");
            let cfg = AuthConfig::from_env("0.0.0.0:3000");
            assert!(cfg.require);
            assert!(cfg.token.is_none());
            if let Some(v) = prior {
                std::env::set_var("SWFC_SERVER_AUTH_TOKEN", v);
            }
        });
    }
}
