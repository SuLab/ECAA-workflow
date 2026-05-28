//! Integration test for the typed `ApiError`
//! envelope. Cross-checks the wire shape (`{ error: { code, message } }`)
//! and the HTTP status emitted by each `ApiError` variant when run
//! through `IntoResponse`.
//!
//! Lives in `crates/server/tests/` so it exercises the public
//! [`scripps_workflow_server::error::ApiError`] surface the rest of
//! the codebase will consume.

use axum::body::Body;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use http_body_util::BodyExt;
use scripps_workflow_server::error::ApiError;

async fn body_json(b: Body) -> serde_json::Value {
    let bytes = b.collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).expect("response body is valid JSON")
}

#[tokio::test]
async fn not_found_envelope() {
    let resp = ApiError::NotFound("session foo not found".into()).into_response();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    let body = body_json(resp.into_body()).await;
    assert_eq!(body["error"]["code"], "not_found");
    assert!(body["error"]["message"]
        .as_str()
        .unwrap()
        .contains("session foo not found"));
}

#[tokio::test]
async fn precondition_failed_envelope() {
    let resp = ApiError::PreconditionFailed("user_confirmed required".into()).into_response();
    assert_eq!(resp.status(), StatusCode::PRECONDITION_FAILED);
    let body = body_json(resp.into_body()).await;
    assert_eq!(body["error"]["code"], "precondition_failure");
    assert!(body["error"]["message"]
        .as_str()
        .unwrap()
        .contains("user_confirmed required"));
}

#[tokio::test]
async fn forbidden_envelope() {
    let resp = ApiError::Forbidden("read-only mode".into()).into_response();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    let body = body_json(resp.into_body()).await;
    assert_eq!(body["error"]["code"], "forbidden");
}

#[tokio::test]
async fn conflict_envelope() {
    let resp = ApiError::Conflict("already emitted".into()).into_response();
    assert_eq!(resp.status(), StatusCode::CONFLICT);
    let body = body_json(resp.into_body()).await;
    assert_eq!(body["error"]["code"], "conflict");
}

#[tokio::test]
async fn bad_request_envelope() {
    let resp = ApiError::BadRequest("missing field foo".into()).into_response();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body = body_json(resp.into_body()).await;
    assert_eq!(body["error"]["code"], "bad_request");
}

#[tokio::test]
async fn body_too_large_envelope() {
    let resp = ApiError::BodyTooLarge {
        limit_bytes: 32 * 1024 * 1024,
    }
    .into_response();
    assert_eq!(resp.status(), StatusCode::PAYLOAD_TOO_LARGE);
    let body = body_json(resp.into_body()).await;
    assert_eq!(body["error"]["code"], "body_too_large");
    assert!(body["error"]["message"]
        .as_str()
        .unwrap()
        .contains("33554432"));
}

#[tokio::test]
async fn rate_limited_envelope() {
    let resp = ApiError::RateLimited.into_response();
    assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS);
    let body = body_json(resp.into_body()).await;
    assert_eq!(body["error"]["code"], "rate_limited");
}

#[tokio::test]
async fn validation_envelope() {
    let resp = ApiError::Validation("negative timeout".into()).into_response();
    assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let body = body_json(resp.into_body()).await;
    assert_eq!(body["error"]["code"], "validation_failed");
}

#[tokio::test]
async fn internal_envelope_wraps_anyhow() {
    let resp = ApiError::Internal(anyhow::anyhow!("boom")).into_response();
    assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let body = body_json(resp.into_body()).await;
    assert_eq!(body["error"]["code"], "internal_error");
    assert!(body["error"]["message"].as_str().unwrap().contains("boom"));
}
