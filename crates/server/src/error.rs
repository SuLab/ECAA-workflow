//! Typed `ApiError` with a stable JSON envelope.
//!
//! Pre-Phase-4, ~117 chat-route handlers each rolled their own error
//! shape, with at least 8 sites disambiguating between `404 / 412 / 413`
//! by `String::contains("not found")` on the underlying `anyhow::Error`.
//! That coupling silently broke any time a downstream error message
//! changed, and there was no contract for the UI side to key off of.
//!
//! This module introduces a typed [`ApiError`] enum and an
//! [`axum::response::IntoResponse`] impl that emits a stable JSON
//! envelope:
//!
//! ```json
//! { "error": { "code": "<machine_id>", "message": "<human prose>" } }
//! ```
//!
//! Each variant maps deterministically to one HTTP status + one stable
//! `code` string. Handlers convert their existing error branches to a
//! typed variant — no more substring matching, no more drift between
//! the `StatusCode` and the body shape.
//!
//! Migration is incremental. The 8 substring-matching sites are
//! migrated in the same PR that introduces this module; the remaining
//! ~109 handlers can switch their return types to
//! `Result<Json<T>, ApiError>` one PR at a time without breaking
//! callers that still parse the legacy `text/plain` body — those still
//! work; the new typed handlers just produce a richer JSON shape that
//! the UI can branch on.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;

/// Closed-vocabulary error type for chat-route handlers.
///
/// Each variant carries the human-facing message inline; the
/// [`IntoResponse`] impl pairs it with a stable HTTP status + a
/// machine-readable `code` string. The latter is what UI code should
/// branch on — the message is for humans only and may evolve over
/// time without breaking the contract.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ApiError {
    /// 404 — the targeted resource (session, task, input,...) does
    /// not exist or has been removed.
    #[error("not found: {0}")]
    NotFound(String),

    /// 412 — a precondition the handler enforces was not met. The
    /// canonical example is the confirm-time `user_confirmed == true`
    /// gate inside `send_turn`; SME-driven recovery flows that depend
    /// on session state (Blocked, ReadyToEmit, …) also surface here.
    #[error("precondition failed: {0}")]
    PreconditionFailed(String),

    /// 403 — the caller is authenticated but lacks authority for the
    /// requested action (e.g. read-only mode, share-token mismatch).
    #[error("forbidden: {0}")]
    Forbidden(String),

    /// 409 — the action would create a duplicate / inconsistent
    /// resource (e.g. session already emitted).
    #[error("conflict: {0}")]
    Conflict(String),

    /// 400 — the request body / path / query is malformed in a way
    /// the handler cannot interpret. Distinct from `Validation`: 400
    /// is for shape errors, 422 is for value errors.
    #[error("bad request: {0}")]
    BadRequest(String),

    /// 413 — the request body exceeded the per-endpoint cap. Carries
    /// the limit so the UI can surface a precise message ("upload
    /// Chunk capped at 32 MiB", "intake prose capped at 64 KiB",...).
    #[error("body too large: {limit_bytes} bytes")]
    BodyTooLarge {
        /// Per-endpoint byte cap that was exceeded.
        limit_bytes: u64,
    },

    /// 429 — the per-IP global governor or a per-session LLM-firing
    /// rate-limit bucket refused the request.
    #[error("rate limited")]
    RateLimited,

    /// 422 — the request was well-formed but its values failed
    /// validation (e.g. negative timeout, unknown enum tag).
    #[error("validation: {0}")]
    Validation(String),

    /// 500 — wraps an unexpected `anyhow::Error` from any internal
    /// dependency. The message is included in the response body for
    /// dev environments; production deployments should strip it via a
    /// fronting proxy if PII is a concern.
    #[error(transparent)]
    Internal(#[from] anyhow::Error),
}

impl ApiError {
    /// Status + machine-readable `code` for the variant. Pulled out as
    /// a helper so both the [`IntoResponse`] impl and any test that
    /// wants to introspect the contract can share one source of truth.
    pub fn status_and_code(&self) -> (StatusCode, &'static str) {
        match self {
            Self::NotFound(_) => (StatusCode::NOT_FOUND, "not_found"),
            Self::PreconditionFailed(_) => {
                (StatusCode::PRECONDITION_FAILED, "precondition_failure")
            }
            Self::Forbidden(_) => (StatusCode::FORBIDDEN, "forbidden"),
            Self::Conflict(_) => (StatusCode::CONFLICT, "conflict"),
            Self::BadRequest(_) => (StatusCode::BAD_REQUEST, "bad_request"),
            Self::BodyTooLarge { .. } => (StatusCode::PAYLOAD_TOO_LARGE, "body_too_large"),
            Self::RateLimited => (StatusCode::TOO_MANY_REQUESTS, "rate_limited"),
            Self::Validation(_) => (StatusCode::UNPROCESSABLE_ENTITY, "validation_failed"),
            Self::Internal(_) => (StatusCode::INTERNAL_SERVER_ERROR, "internal_error"),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, code) = self.status_and_code();
        let body = serde_json::json!({
            "error": {
                "code": code,
                "message": self.to_string(),
            }
        });
        (status, Json(body)).into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_variant_maps_to_a_distinct_status_and_code() {
        // The JSON-envelope contract pinned by
        // this test is what the UI will branch on. Drift in either the
        // `code` string or the status code is a breaking API change.
        let cases = [
            (
                ApiError::NotFound("x".into()),
                StatusCode::NOT_FOUND,
                "not_found",
            ),
            (
                ApiError::PreconditionFailed("x".into()),
                StatusCode::PRECONDITION_FAILED,
                "precondition_failure",
            ),
            (
                ApiError::Forbidden("x".into()),
                StatusCode::FORBIDDEN,
                "forbidden",
            ),
            (
                ApiError::Conflict("x".into()),
                StatusCode::CONFLICT,
                "conflict",
            ),
            (
                ApiError::BadRequest("x".into()),
                StatusCode::BAD_REQUEST,
                "bad_request",
            ),
            (
                ApiError::BodyTooLarge { limit_bytes: 1024 },
                StatusCode::PAYLOAD_TOO_LARGE,
                "body_too_large",
            ),
            (
                ApiError::RateLimited,
                StatusCode::TOO_MANY_REQUESTS,
                "rate_limited",
            ),
            (
                ApiError::Validation("x".into()),
                StatusCode::UNPROCESSABLE_ENTITY,
                "validation_failed",
            ),
            (
                ApiError::Internal(anyhow::anyhow!("boom")),
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal_error",
            ),
        ];
        for (variant, expected_status, expected_code) in cases {
            let (status, code) = variant.status_and_code();
            assert_eq!(
                status, expected_status,
                "status drifted for code={expected_code}"
            );
            assert_eq!(
                code, expected_code,
                "code string drifted for status={expected_status:?}"
            );
        }
    }
}
