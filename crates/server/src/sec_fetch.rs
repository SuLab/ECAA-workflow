//! Sec-Fetch-Site enforcement on state-mutating browser requests.
//!
//! Modern browsers attach `Sec-Fetch-Site: cross-site` on requests
//! initiated from a page on a different origin than the target.
//! Rejecting those for POST/PUT/DELETE/PATCH adds defense-in-depth
//! against CSRF from a malicious page even if CORS were misconfigured
//! or the same browser held a stale auth cookie.
//!
//! Non-browser clients (`curl`, the harness, the agent) don't send
//! `Sec-Fetch-*` at all; missing-header requests pass through so the
//! orchestrator paths keep working.

use axum::{
    body::Body,
    http::{Method, Request, StatusCode},
    middleware::Next,
    response::Response,
};

/// Axum middleware that rejects `cross-site` `Sec-Fetch-Site` requests
/// on state-mutating HTTP methods (POST/PUT/DELETE/PATCH).
pub async fn sec_fetch_guard(req: Request<Body>, next: Next) -> Response {
    // Only guard state-mutating methods. GET/HEAD/OPTIONS are visible
    // to the same-origin policy anyway.
    if !matches!(
        req.method(),
        &Method::POST | &Method::PUT | &Method::DELETE | &Method::PATCH
    ) {
        return next.run(req).await;
    }
    // Only apply when the request looks browser-shaped. curl/harness
    // don't send Sec-Fetch-*.
    let Some(site) = req
        .headers()
        .get("sec-fetch-site")
        .and_then(|h| h.to_str().ok())
    else {
        return next.run(req).await;
    };
    // Allow: none (address bar), same-origin, same-site. Reject
    // cross-site. Unknown future values pass through — Fetch Metadata
    // is allow-listy by intent.
    if matches!(site, "cross-site") {
        let mut resp = Response::new(Body::from(r#"{"error":"cross-site request blocked"}"#));
        *resp.status_mut() = StatusCode::FORBIDDEN;
        resp.headers_mut()
            .insert("content-type", "application/json".parse().unwrap());
        return resp;
    }
    next.run(req).await
}
