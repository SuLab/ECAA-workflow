use axum::{
    body::Body,
    http::{Method, Request, StatusCode},
};
use ecaa_workflow_server::sec_fetch::sec_fetch_guard;
use tower::ServiceExt;

fn app() -> axum::Router {
    use axum::middleware::from_fn;
    use axum::routing::post;
    axum::Router::new()
        .route("/api/x", post(|| async { "ok" }))
        .layer(from_fn(sec_fetch_guard))
}

#[tokio::test]
async fn cross_site_post_rejected() {
    let resp = app()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/api/x")
                .header("Sec-Fetch-Site", "cross-site")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn same_origin_post_allowed() {
    let resp = app()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/api/x")
                .header("Sec-Fetch-Site", "same-origin")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn missing_header_allowed_for_non_browser_clients() {
    // curl, harness, etc. don't send Sec-Fetch-* — we allow these.
    let resp = app()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/api/x")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn none_value_allowed_for_address_bar() {
    let resp = app()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/api/x")
                .header("Sec-Fetch-Site", "none")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn same_site_post_allowed() {
    let resp = app()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/api/x")
                .header("Sec-Fetch-Site", "same-site")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}
