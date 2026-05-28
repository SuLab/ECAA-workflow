//! JSON body extractor with a depth limit. Wraps Axum's `Json<T>` so
//! the deserializer can't be tricked into recursing past
//! `MAX_JSON_DEPTH` levels, which would otherwise let a hostile or
//! malformed client stack-overflow the request thread.
//!
//! Apply on high-impact mutating routes (`/confirm`, `/reject`,
//! `/unblock`, `/branch`, `/turn`, `/start_execution`, the propose-*
//! endpoints, `/sessions`). Read-only routes that don't take a JSON
//! body don't need this.
//!
//! Implementation strategy: parse the body once into
//! `serde_json::Value`, walk it once to assert depth, then re-deserialize
//! into the target type. The double-parse is intentional — it lets us
//! reject pathological inputs BEFORE the strongly-typed deserialization
//! recurses with the on-stack visitor that would otherwise blow the
//! stack. The walking pass uses an explicit stack, so the depth check
//! itself cannot stack-overflow.

use axum::{
    async_trait,
    body::Bytes,
    extract::{FromRequest, Request},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::de::DeserializeOwned;

/// Maximum allowed JSON object/array nesting depth. Beyond this we
/// return 400. The Anthropic Messages API caps tool-input nesting at
/// ~16; we leave generous headroom for `decisions.jsonl`-style
/// auditing payloads but stay far below the 1 MiB default stack
/// (deserializing a 32-deep object via serde_json's recursive
/// visitor stays under ~32 KiB on x86-64).
pub const MAX_JSON_DEPTH: u8 = 32;

/// Walk `value` non-recursively, asserting the maximum nesting depth
/// is at most `MAX_JSON_DEPTH`. Iterative so the check itself can't
/// stack-overflow on adversarial input.
fn check_depth(root: &serde_json::Value) -> Result<(), &'static str> {
    let mut stack: Vec<(&serde_json::Value, u8)> = Vec::with_capacity(32);
    stack.push((root, 0));
    while let Some((value, depth)) = stack.pop() {
        if depth > MAX_JSON_DEPTH {
            return Err("json depth exceeded");
        }
        match value {
            serde_json::Value::Object(map) => {
                for (_, v) in map {
                    stack.push((v, depth + 1));
                }
            }
            serde_json::Value::Array(arr) => {
                for v in arr {
                    stack.push((v, depth + 1));
                }
            }
            _ => {}
        }
    }
    Ok(())
}

/// Drop-in replacement for `axum::Json<T>` that rejects body payloads
/// whose nesting depth exceeds `MAX_JSON_DEPTH`. On reject, returns
/// HTTP 400 with the failure reason.
pub struct BoundedJson<T>(pub T);

#[async_trait]
impl<S, T> FromRequest<S> for BoundedJson<T>
where
    S: Send + Sync,
    T: DeserializeOwned,
{
    type Rejection = Response;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        let bytes = Bytes::from_request(req, state)
            .await
            .map_err(IntoResponse::into_response)?;
        if bytes.is_empty() {
            // Match Axum's `Json<T>` behavior: empty body is 415 / 422
            // depending on the route shape. Returning 400 here is
            // intentional — the high-impact mutation routes that opt
            // into BoundedJson all require a body.
            return Err((StatusCode::BAD_REQUEST, "request body is empty").into_response());
        }
        let value: serde_json::Value = serde_json::from_slice(&bytes)
            .map_err(|e| (StatusCode::BAD_REQUEST, format!("invalid json: {e}")).into_response())?;
        if let Err(msg) = check_depth(&value) {
            return Err((StatusCode::BAD_REQUEST, msg).into_response());
        }
        let typed: T = serde_json::from_value(value).map_err(|e| {
            (StatusCode::BAD_REQUEST, format!("json shape mismatch: {e}")).into_response()
        })?;
        Ok(Self(typed))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flat_object_passes() {
        let v = serde_json::json!({ "a": 1, "b": "two", "c": [1, 2, 3] });
        assert!(check_depth(&v).is_ok());
    }

    #[test]
    fn shallow_nested_passes() {
        let v = serde_json::json!({ "a": { "b": { "c": { "d": 1 } } } });
        assert!(check_depth(&v).is_ok());
    }

    #[test]
    fn at_limit_passes() {
        // Build an object nested exactly MAX_JSON_DEPTH deep.
        let mut v = serde_json::Value::Number(1.into());
        for _ in 0..MAX_JSON_DEPTH {
            let mut map = serde_json::Map::new();
            map.insert("k".into(), v);
            v = serde_json::Value::Object(map);
        }
        assert!(check_depth(&v).is_ok());
    }

    #[test]
    fn beyond_limit_rejected() {
        let mut v = serde_json::Value::Number(1.into());
        for _ in 0..(MAX_JSON_DEPTH as usize + 2) {
            let mut map = serde_json::Map::new();
            map.insert("k".into(), v);
            v = serde_json::Value::Object(map);
        }
        assert!(check_depth(&v).is_err());
    }

    #[test]
    fn deep_array_rejected() {
        let mut v = serde_json::Value::Number(1.into());
        for _ in 0..(MAX_JSON_DEPTH as usize + 2) {
            v = serde_json::Value::Array(vec![v]);
        }
        assert!(check_depth(&v).is_err());
    }

    #[test]
    fn depth_check_uses_explicit_stack_not_recursion() {
        // 10_000-deep array would blow the stack via recursive descent.
        // Iterative check handles it.
        let mut v = serde_json::Value::Number(1.into());
        for _ in 0..10_000 {
            v = serde_json::Value::Array(vec![v]);
        }
        // Expected: rejected (way over MAX_JSON_DEPTH), but the
        // function returns, doesn't crash.
        assert_eq!(check_depth(&v), Err("json depth exceeded"));
    }
}
