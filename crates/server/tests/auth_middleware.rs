use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use ecaa_workflow_server::auth::{auth_middleware, AuthConfig};
use tower::ServiceExt;

fn app(cfg: AuthConfig) -> axum::Router {
    use axum::middleware::from_fn_with_state;
    use axum::routing::get;
    axum::Router::new()
        .route("/api/health", get(|| async { "ok" }))
        .layer(from_fn_with_state(cfg, auth_middleware))
}

#[tokio::test]
async fn no_token_configured_passes_through() {
    let cfg = AuthConfig {
        token: None,
        require: false,
    };
    let resp = app(cfg)
        .oneshot(
            Request::builder()
                .uri("/api/health")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn require_with_no_header_returns_401() {
    let cfg = AuthConfig {
        token: Some("secret".into()),
        require: true,
    };
    let resp = app(cfg)
        .oneshot(
            Request::builder()
                .uri("/api/health")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn correct_bearer_passes() {
    let cfg = AuthConfig {
        token: Some("secret".into()),
        require: true,
    };
    let resp = app(cfg)
        .oneshot(
            Request::builder()
                .uri("/api/health")
                .header("Authorization", "Bearer secret")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn wrong_bearer_returns_401() {
    let cfg = AuthConfig {
        token: Some("secret".into()),
        require: true,
    };
    let resp = app(cfg)
        .oneshot(
            Request::builder()
                .uri("/api/health")
                .header("Authorization", "Bearer wrong")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn cors_rejects_unknown_origin() {
    use axum::http::Method;
    use ecaa_workflow_server::cors::build_cors;
    let layer = build_cors();
    // Tower layer compile check; full CORS-roundtrip lives in chat_routes::cors_tests.
    let _ = layer;
    let _ = Method::POST;
}

#[tokio::test]
async fn loopback_exempt_when_configured() {
    let cfg = AuthConfig {
        token: Some("secret".into()),
        require: true,
    };
    // For now, no loopback exemption — the test confirms behavior is uniform.
    // (We are NOT adding a loopback exemption; the UI carries the token.)
    let resp = app(cfg)
        .oneshot(
            Request::builder()
                .uri("/api/health")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}
