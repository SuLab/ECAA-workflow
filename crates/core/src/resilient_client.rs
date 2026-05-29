//! Resilient HTTP client wrapper with HTTPS scheme enforcement and
//! structured tracing.
//!
//! Ships the URL-validation guard + client builder. The
//! exponential-backoff retry + circuit-breaker layer is a follow-up.
//! The Anthropic SSE `\r\n\r\n` boundary fix is paired with this and
//! lives in `crates/conversation/src/anthropic/stream.rs`.

use thiserror::Error;
use url::Url;

#[derive(Debug, Error)]
/// ResilientClientError discriminant.
pub enum ResilientClientError {
    #[error(
        "URL must be https:// (got {scheme}://{host}); loopback is the only http:// exception"
    )]
    /// Variant.
    /// Field value.
    /// Field value.
    InsecureScheme { scheme: String, host: String },
    #[error("URL parse failed: {0}")]
    /// UrlParse variant.
    UrlParse(#[from] url::ParseError),
    #[error("reqwest builder: {0}")]
    /// Reqwest variant.
    Reqwest(#[from] reqwest::Error),
}

/// Configuration for a `ResilientClient`.
#[derive(Debug, Clone)]
pub struct ResilientClientConfig {
    /// Base url.
    pub base_url: Url,
    /// Timeout.
    pub timeout: std::time::Duration,
    /// User agent.
    pub user_agent: String,
    /// Per-connection-attempt TCP connect timeout. Applied to each
    /// resolved address individually (hyper-util `HttpConnector`
    /// semantics): when a host resolves to both an A and a dead AAAA
    /// record, the dead IPv6 attempt fails after this bound and reqwest
    /// falls back to the IPv4 address instead of blocking on the full
    /// kernel SYN-retransmission window (~127 s on default Linux). Keep
    /// it well above any healthy connect (<1 s) but well under the
    /// kernel timeout so broken-IPv6 hosts stay responsive.
    pub connect_timeout: std::time::Duration,
}

impl Default for ResilientClientConfig {
    fn default() -> Self {
        Self {
            base_url: Url::parse("https://api.anthropic.com").expect("constant"),
            timeout: std::time::Duration::from_secs(300),
            user_agent: format!("ecaa-workflow/{}", env!("CARGO_PKG_VERSION")),
            connect_timeout: std::time::Duration::from_secs(10),
        }
    }
}

/// HTTP client wrapper enforcing HTTPS-or-loopback scheme.
///
/// Construct via [`ResilientClient::new`] which rejects non-https
/// base URLs unless the host is loopback (localhost / 127.0.0.1 / ::1).
/// Loopback is permitted for development / testing only (e.g., a mock
/// server bound to 127.0.0.1:8080).
#[derive(Debug, Clone)]
pub struct ResilientClient {
    inner: reqwest::Client,
    config: ResilientClientConfig,
}

impl ResilientClient {
    /// New.
    pub fn new(config: ResilientClientConfig) -> Result<Self, ResilientClientError> {
        validate_scheme(&config.base_url)?;
        let inner = reqwest::Client::builder()
            .timeout(config.timeout)
            .connect_timeout(config.connect_timeout)
            .user_agent(&config.user_agent)
            .build()?;
        Ok(ResilientClient { inner, config })
    }

    /// Base url.
    pub fn base_url(&self) -> &Url {
        &self.config.base_url
    }

    /// Inner.
    pub fn inner(&self) -> &reqwest::Client {
        &self.inner
    }
}

/// Reject non-https URLs unless the host is loopback. The sync variant
/// for the harness lives in `harness::resilient_sync`.
pub fn validate_scheme(url: &Url) -> Result<(), ResilientClientError> {
    if url.scheme() == "https" {
        return Ok(());
    }
    let host = url.host_str().unwrap_or("").to_string();
    if matches!(host.as_str(), "localhost" | "127.0.0.1" | "::1" | "[::1]") {
        return Ok(());
    }
    Err(ResilientClientError::InsecureScheme {
        scheme: url.scheme().to_string(),
        host,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(base: &str) -> ResilientClientConfig {
        ResilientClientConfig {
            base_url: Url::parse(base).unwrap(),
            ..ResilientClientConfig::default()
        }
    }

    #[test]
    fn accepts_https() {
        assert!(ResilientClient::new(cfg("https://api.anthropic.com")).is_ok());
    }

    #[test]
    fn rejects_http_non_loopback() {
        let err = ResilientClient::new(cfg("http://api.anthropic.com")).unwrap_err();
        assert!(matches!(err, ResilientClientError::InsecureScheme { .. }));
    }

    #[test]
    fn accepts_loopback_http() {
        for h in [
            "http://localhost:8080",
            "http://127.0.0.1:3000",
            "http://[::1]:8080/",
        ] {
            assert!(
                ResilientClient::new(cfg(h)).is_ok(),
                "should accept loopback: {h}"
            );
        }
    }

    #[test]
    fn rejects_other_schemes() {
        for bad in ["ftp://example.com", "file:///tmp/x", "ext::sh"] {
            // Some of these may fail at url::Url::parse — that's also fine.
            let parsed = Url::parse(bad);
            if let Ok(u) = parsed {
                assert!(validate_scheme(&u).is_err(), "should reject {bad}");
            }
        }
    }
}
