//! CORS layer for the chat server.
//!
//! Allow-list sourced from `ECAA_CORS_ORIGINS` (comma-separated). The
//! default covers the standard local-dev origins; production
//! deployments must set `ECAA_CORS_ORIGINS` to their actual UI
//! host(s). Permissive CORS is never the default.

use axum::http::{HeaderValue, Method};
use tower_http::cors::{AllowOrigin, CorsLayer};

/// Build the CORS layer from `ECAA_CORS_ORIGINS` (comma-separated list of allowed origins).
/// Defaults to the standard local-dev origins when the env var is absent.
pub fn build_cors() -> CorsLayer {
    let origins = std::env::var("ECAA_CORS_ORIGINS").unwrap_or_else(|_| {
        "http://localhost:5173,http://127.0.0.1:5173,\
             http://localhost:3000,http://127.0.0.1:3000"
            .to_string()
    });
    let parsed: Vec<HeaderValue> = origins
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .filter_map(|s| s.parse::<HeaderValue>().ok())
        .collect();

    CorsLayer::new()
        .allow_origin(AllowOrigin::list(parsed))
        .allow_methods([
            Method::GET,
            Method::POST,
            Method::PUT,
            Method::DELETE,
            Method::OPTIONS,
        ])
        .allow_headers([
            "authorization".parse().unwrap(),
            "content-type".parse().unwrap(),
            "x-share-token".parse().unwrap(),
            "x-scripps-user".parse().unwrap(),
        ])
        .allow_credentials(false)
        .max_age(std::time::Duration::from_secs(600))
}
