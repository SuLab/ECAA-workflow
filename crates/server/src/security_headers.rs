//! Global response
//! header stamper that closes the missing-security-headers finding from
//! the multi-agent audit.
//!
//! Layered as the outermost `axum::middleware::from_fn` in
//! [`crate::run`] so every response — REST, SSE, the bundled UI under
//! `ui/dist`, error pages — picks up:
//!
//! - **Content-Security-Policy** — locks `script-src`/`style-src`/etc.
//!   to same-origin. `connect-src 'self'` covers same-origin SSE
//!   (`/api/chat/session/:id/events`). `frame-ancestors 'none'` is the
//!   CSP equivalent of `X-Frame-Options: DENY`; the redundancy is
//!   intentional for legacy browsers. `style-src 'self' 'unsafe-inline'`
//!   tolerates React 18's runtime inline styles; switching to a hash-
//!   based allowlist is plan-amendment future work.
//! - **X-Frame-Options: DENY** — defense in depth for clickjacking.
//! - **X-Content-Type-Options: nosniff** — prevents browsers from
//!   re-interpreting `Content-Type` on artifact downloads.
//! - **Referrer-Policy: no-referrer** — keeps session IDs out of
//!   third-party referer logs (note: with auth + same-origin this is
//!   already private, but the header costs nothing).
//! - **Permissions-Policy** — affirmatively denies geolocation /
//!   microphone / camera. The UI never asks for any of these; the
//!   header is a backstop against future regression or a compromised
//!   bundle.
//!
//! The CSP policy is identical for HTML and JSON responses. JSON
//! payloads aren't rendered as documents but stamping anyway keeps the
//! middleware uniform — clients ignore the header on non-document
//! responses.

use axum::{body::Body, http::Request, middleware::Next, response::Response};

/// Wrap `next.run(req)` and stamp the response with the canonical
/// security-header set. Idempotent — if an upstream handler already
/// emitted any of these headers we overwrite (this is the contract on
/// `HeaderMap::insert`), so a single canonical policy survives.
pub async fn security_headers_middleware(req: Request<Body>, next: Next) -> Response {
    let mut resp = next.run(req).await;
    let h = resp.headers_mut();
    // Standard security-header policy.
    // `connect-src 'self'` is load-bearing for SSE; do not loosen.
    h.insert(
        "content-security-policy",
        "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; \
         img-src 'self' data:; connect-src 'self'; frame-ancestors 'none'; \
         base-uri 'self'; form-action 'self'"
            .parse()
            .expect("static CSP header is valid"),
    );
    h.insert(
        "x-frame-options",
        "DENY".parse().expect("static header is valid"),
    );
    h.insert(
        "x-content-type-options",
        "nosniff".parse().expect("static header is valid"),
    );
    h.insert(
        "referrer-policy",
        "no-referrer".parse().expect("static header is valid"),
    );
    h.insert(
        "permissions-policy",
        "geolocation=(), microphone=(), camera=()"
            .parse()
            .expect("static header is valid"),
    );
    resp
}
