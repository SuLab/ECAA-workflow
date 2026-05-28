//! Input deletion.
//!
//! Owns:
//! - `DELETE /api/chat/session/:id/inputs/:input_id` (`delete_input`)
//!
//! Removes a registration from `Session.inputs`. Does NOT delete the
//! underlying files — for `local_path` inputs that's the SME's data;
//! for `uploaded_files` inputs the file deletion happens in a separate
//! pass once the upload registry tracks live references vs garbage.

use super::ChatAppState;
use axum::extract::Path;
use axum::{extract::State, response::IntoResponse, Json};
use uuid::Uuid;

/// `DELETE /api/chat/session/:id/inputs/:input_id`
///
/// Idempotent: a second DELETE of the same `input_id` returns the
/// current inputs list with HTTP 200 (the resource is in the desired
/// "absent" state). RFC 9110 §9.3.5 explicitly permits 200/204 on a
/// non-existent target so long as the client can observe the desired
/// post-condition; we choose 200-with-list so the UI's optimistic
/// retry on a flaky network produces the same surface every time
/// (closes P3-189). Session-not-found still yields a real 404.
pub(crate) async fn delete_input(
    State(app): State<ChatAppState>,
    Path((session_id, input_id)): Path<(Uuid, String)>,
) -> impl IntoResponse {
    let store = app.conversation.store_handle();
    let result = store
        .update(session_id, move |s| {
            // No-op when input_id is already absent; the closure
            // simply returns success so the second DELETE observes
            // the current empty/post-delete state without 404'ing.
            s.inputs.retain(|i| i.input_id != input_id);
            Ok(())
        })
        .await;
    match result {
        Ok(s) => Json(s.inputs.clone()).into_response(),
        Err(e) => {
            let msg = e.to_string();
            // The only remaining failure mode is "session not found"
            // (the closure can't fail otherwise). Surface as typed
            // `ApiError::NotFound`.
            if msg.contains("not found") {
                crate::error::ApiError::NotFound(msg).into_response()
            } else {
                crate::error::ApiError::Internal(anyhow::anyhow!(msg)).into_response()
            }
        }
    }
}

/// Per-file route inventory + builder. Documented next to its handler
/// list and used by the compile-time consistency assert in
/// `super::mod.rs` to ensure the aggregate `super::ROUTES` doesn't
/// drift from what each submodule serves.
#[allow(dead_code)] // doc-as-contract gate; consumed by const _: () assert.
pub(super) const ROUTES: &[(&str, &str)] = &[("DELETE", "/api/chat/session/:id/inputs/:input_id")];

pub(super) fn routes() -> axum::Router<ChatAppState> {
    axum::Router::new().route(
        "/api/chat/session/:id/inputs/:input_id",
        axum::routing::delete(delete_input),
    )
}

#[cfg(test)]
mod tests {
    use crate::chat_routes::inputs::test_helpers::allowlisted_temp;
    use crate::chat_routes::test_support::{body_json, make_router};
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::util::ServiceExt;

    #[tokio::test]
    async fn delete_input_removes_one_registration() {
        let root = allowlisted_temp();
        let (router, app) = make_router(vec![]).await;
        let (id, _) = app.conversation.start_session(false).await.unwrap();

        // Register.
        let body = serde_json::json!({ "path": root.display().to_string() });
        let req = Request::builder()
            .method("POST")
            .uri(format!("/api/chat/session/{}/inputs/path", id))
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap();
        let resp = router.clone().oneshot(req).await.unwrap();
        let arr = body_json(resp.into_body()).await;
        let input_id = arr[0]["input_id"].as_str().unwrap().to_string();

        // Delete.
        let req = Request::builder()
            .method("DELETE")
            .uri(format!("/api/chat/session/{}/inputs/{}", id, input_id))
            .body(Body::empty())
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let session = app.conversation.get_session(id).await.unwrap();
        assert!(session.inputs.is_empty(), "input removed from session");
    }

    #[tokio::test]
    async fn delete_unknown_input_is_idempotent() {
        // P3-189: DELETE on an already-absent input is idempotent —
        // returns 200 with the current (empty) inputs list, NOT 404.
        // The session is the resource; the input slot is just one
        // element. Second DELETE observes the desired post-condition
        // (the input is gone) and reports success.
        let _ = allowlisted_temp();
        let (router, app) = make_router(vec![]).await;
        let (id, _) = app.conversation.start_session(false).await.unwrap();

        let req = Request::builder()
            .method("DELETE")
            .uri(format!("/api/chat/session/{}/inputs/abcdef0123456789", id))
            .body(Body::empty())
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }
}
