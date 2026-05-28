//! Verify the global
//! `security_headers_middleware` stamps CSP, X-Frame-Options,
//! X-Content-Type-Options, Referrer-Policy, and Permissions-Policy on
//! every response.

use axum::{body::Body, http::Request};
use ecaa_workflow_server::security_headers::security_headers_middleware;
use tower::ServiceExt;

#[tokio::test]
async fn csp_header_present() {
    let app = axum::Router::new()
        .route("/x", axum::routing::get(|| async { "ok" }))
        .layer(axum::middleware::from_fn(security_headers_middleware));
    let resp = app
        .oneshot(Request::builder().uri("/x").body(Body::empty()).unwrap())
        .await
        .unwrap();
    let csp = resp
        .headers()
        .get("content-security-policy")
        .expect("CSP header must be set");
    let csp_str = csp.to_str().unwrap();
    assert!(
        csp_str.contains("default-src 'self'"),
        "CSP must contain default-src 'self', got {}",
        csp_str
    );
    assert!(
        csp_str.contains("frame-ancestors 'none'"),
        "CSP must contain frame-ancestors 'none', got {}",
        csp_str
    );
    assert!(
        csp_str.contains("base-uri 'self'"),
        "CSP must contain base-uri 'self', got {}",
        csp_str
    );
    assert!(
        csp_str.contains("form-action 'self'"),
        "CSP must contain form-action 'self', got {}",
        csp_str
    );
    assert!(
        csp_str.contains("connect-src 'self'"),
        "CSP must contain connect-src 'self' (covers same-origin SSE), got {}",
        csp_str
    );
    assert_eq!(resp.headers().get("x-frame-options").unwrap(), "DENY");
    assert_eq!(
        resp.headers().get("referrer-policy").unwrap(),
        "no-referrer"
    );
    assert_eq!(
        resp.headers().get("x-content-type-options").unwrap(),
        "nosniff"
    );
    let pp = resp
        .headers()
        .get("permissions-policy")
        .expect("Permissions-Policy must be set");
    let pp_str = pp.to_str().unwrap();
    assert!(
        pp_str.contains("geolocation=()"),
        "Permissions-Policy must lock geolocation, got {}",
        pp_str
    );
    assert!(
        pp_str.contains("microphone=()"),
        "Permissions-Policy must lock microphone, got {}",
        pp_str
    );
    assert!(
        pp_str.contains("camera=()"),
        "Permissions-Policy must lock camera, got {}",
        pp_str
    );
}
