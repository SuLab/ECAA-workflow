//! Sync HTTP client wrapper enforcing HTTPS-or-loopback scheme.
//!
//! Sync analogue of
//! `ecaa_workflow_core::resilient_client::ResilientClient`, using
//! ureq (matches the harness's existing HTTP stack).

use thiserror::Error;
use url::Url;

#[derive(Debug, Error)]
/// Errors returned by `ResilientSyncClient` construction and scheme validation.
pub enum ResilientSyncError {
    /// The supplied URL uses an insecure scheme on a non-loopback host.
    #[error(
        "URL must be https:// (got {scheme}://{host}); loopback is the only http:// exception"
    )]
    InsecureScheme {
        /// Scheme that was rejected (e.g. "http").
        scheme: String,
        /// Host that triggered the rejection.
        host: String,
    },
    /// The URL string could not be parsed.
    #[error("URL parse failed: {0}")]
    UrlParse(#[from] url::ParseError),
}

#[derive(Debug, Clone)]
/// Configuration for `ResilientSyncClient`.
pub struct ResilientSyncConfig {
    /// Base URL (scheme + host + port) for all requests. Must be HTTPS or loopback.
    pub base_url: Url,
    /// End-to-end request timeout applied to the `ureq::Agent`.
    pub timeout: std::time::Duration,
    /// `User-Agent` header value sent with every request.
    pub user_agent: String,
}

impl Default for ResilientSyncConfig {
    fn default() -> Self {
        Self {
            base_url: Url::parse("https://localhost").expect("constant"),
            timeout: std::time::Duration::from_secs(30),
            user_agent: format!("ecaa-workflow-harness/{}", env!("CARGO_PKG_VERSION")),
        }
    }
}

#[derive(Clone)]
/// Sync `ureq`-backed HTTP client that enforces HTTPS (or loopback) on construction.
pub struct ResilientSyncClient {
    inner: ureq::Agent,
    config: ResilientSyncConfig,
}

impl std::fmt::Debug for ResilientSyncClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResilientSyncClient")
            .field("config", &self.config)
            .finish()
    }
}

impl ResilientSyncClient {
    /// Constructs the client, validating that `config.base_url` uses HTTPS or a loopback host.
    pub fn new(config: ResilientSyncConfig) -> Result<Self, ResilientSyncError> {
        validate_scheme(&config.base_url)?;
        let inner = ureq::AgentBuilder::new()
            .timeout(config.timeout)
            .user_agent(&config.user_agent)
            .build();
        Ok(ResilientSyncClient { inner, config })
    }

    /// Returns the configured base URL.
    pub fn base_url(&self) -> &Url {
        &self.config.base_url
    }

    /// Returns a reference to the underlying `ureq::Agent` for direct use.
    pub fn inner(&self) -> &ureq::Agent {
        &self.inner
    }
}

/// Validates that `url` uses HTTPS, or that its host is a loopback address.
pub fn validate_scheme(url: &Url) -> Result<(), ResilientSyncError> {
    if url.scheme() == "https" {
        return Ok(());
    }
    let host = url.host_str().unwrap_or("").to_string();
    if matches!(host.as_str(), "localhost" | "127.0.0.1" | "::1" | "[::1]") {
        return Ok(());
    }
    Err(ResilientSyncError::InsecureScheme {
        scheme: url.scheme().to_string(),
        host,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(base: &str) -> ResilientSyncConfig {
        ResilientSyncConfig {
            base_url: Url::parse(base).unwrap(),
            ..ResilientSyncConfig::default()
        }
    }

    #[test]
    fn accepts_https() {
        assert!(ResilientSyncClient::new(cfg("https://localhost:3000")).is_ok());
    }

    #[test]
    fn rejects_http_remote() {
        let err = ResilientSyncClient::new(cfg("http://remote.example.com")).unwrap_err();
        assert!(matches!(err, ResilientSyncError::InsecureScheme { .. }));
    }

    #[test]
    fn accepts_loopback_http() {
        for h in ["http://127.0.0.1:3000", "http://localhost:8080"] {
            assert!(ResilientSyncClient::new(cfg(h)).is_ok());
        }
    }
}
