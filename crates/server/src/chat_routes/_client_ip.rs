//! Client-IP extraction helper.
//!
//! Centralizes the X-Forwarded-For → ConnectInfo<SocketAddr> fallback
//! used by SME-action handlers so the audit-trail population path stays
//! DRY and consistent with the `build_trace_layer` heuristic in
//! `crates/server/src/lib.rs`.
//!
//! Used by handlers that call `Session::record_decision_with_ip` with
//! `DecisionActor::Sme` so the on-disk `decisions.jsonl` carries the
//! originating client IP. LLM-side / harness-side decision recording
//! continues to pass `None`.

use axum::extract::ConnectInfo;
use axum::http::HeaderMap;
use std::net::SocketAddr;

/// Resolve the client IP for an inbound HTTP request, in the same
/// order the `TraceLayer` span uses:
///
/// 1. `X-Forwarded-For` header (when set — operator behind a trusted
///    reverse proxy must opt in; tower_governor's per-IP key extractor
///    intentionally does NOT honour this header, but the audit trail
///    has weaker integrity requirements).
/// 2. `ConnectInfo<SocketAddr>` extension (populated by
///    `into_make_service_with_connect_info::<SocketAddr>()` in
///    `lib.rs::run`).
/// 3. `None` — synthetic requests / unit tests that don't wire the
///    extension. Downstream callers will skip the field in the
///    serialized record (the field is
///    `#[serde(skip_serializing_if = "Option::is_none")]`).
pub fn client_ip_from(
    headers: &HeaderMap,
    connect_info: Option<&ConnectInfo<SocketAddr>>,
) -> Option<String> {
    if let Some(value) = headers.get("x-forwarded-for") {
        if let Ok(s) = value.to_str() {
            let first = s.split(',').next().unwrap_or(s).trim();
            if !first.is_empty() {
                return Some(first.to_string());
            }
        }
    }
    connect_info.map(|ConnectInfo(addr)| addr.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    #[test]
    fn xff_first_value_wins() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-forwarded-for",
            HeaderValue::from_static("203.0.113.42, 10.0.0.1"),
        );
        let peer: SocketAddr = "127.0.0.1:1234".parse().unwrap();
        let ci = ConnectInfo(peer);
        let ip = client_ip_from(&headers, Some(&ci));
        assert_eq!(ip.as_deref(), Some("203.0.113.42"));
    }

    #[test]
    fn falls_back_to_connect_info_when_no_xff() {
        let headers = HeaderMap::new();
        let peer: SocketAddr = "198.51.100.5:54321".parse().unwrap();
        let ci = ConnectInfo(peer);
        let ip = client_ip_from(&headers, Some(&ci));
        assert_eq!(ip.as_deref(), Some("198.51.100.5:54321"));
    }

    #[test]
    fn empty_xff_falls_through_to_connect_info() {
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-for", HeaderValue::from_static(""));
        let peer: SocketAddr = "198.51.100.5:54321".parse().unwrap();
        let ci = ConnectInfo(peer);
        let ip = client_ip_from(&headers, Some(&ci));
        assert_eq!(ip.as_deref(), Some("198.51.100.5:54321"));
    }

    #[test]
    fn returns_none_when_neither_source_set() {
        let headers = HeaderMap::new();
        let ip = client_ip_from(&headers, None);
        assert!(ip.is_none());
    }
}
