//! GET /api/chat/session/:id/literature-context — read-only;
//! no SSE; no git-hook fire.
//!
//! Status codes:
//! 200 — success. Empty prior_rows + finding_rows is still 200.
//! 404 — session not found OR read_literature_context returns NoLiteratureAtoms.
//! 409 — session exists but emitted_package_path is None (not yet emitted).
//! 500 — CsvSchemaMismatch from the reader (logged + opaque to client).

use super::*;
use axum::{extract::Query, extract::State, http::StatusCode, response::IntoResponse, Json};
use ecaa_workflow_conversation::tools::literature_context::{
    read_literature_context, EntityKind, LiteratureContextError,
};
use serde::Deserialize;
use uuid::Uuid;

#[derive(Debug, Deserialize)]
pub(super) struct LiteratureContextQuery {
    pub entity: String,
    pub entity_kind: Option<EntityKind>,
}

pub(super) async fn get_literature_context(
    State(app): State<ChatAppState>,
    Path(session_id): Path<Uuid>,
    Query(q): Query<LiteratureContextQuery>,
) -> impl IntoResponse {
    // 1. Load the session; return 404 if not found.
    let Some(session) = app.conversation.get_session(session_id).await else {
        return (StatusCode::NOT_FOUND, "session not found").into_response();
    };

    // 2. Require an emitted package; return 409 with state if absent.
    let Some(pkg_root) = session.emitted_package_path.clone() else {
        let state_kind = session_state_kind(&session.state);
        return (
            StatusCode::CONFLICT,
            Json(serde_json::json!({
                "error": "package not yet emitted",
                "state": state_kind,
            })),
        )
            .into_response();
    };

    // 3. Read literature context from the package on a blocking thread.
    // read_literature_context does its own safe path construction from
    // pkg_root — no user input reaches the filesystem here directly.
    let entity = q.entity.clone();
    let entity_kind = q.entity_kind.clone();
    let result = tokio::task::spawn_blocking(move || {
        read_literature_context(&pkg_root, &entity, entity_kind)
    })
    .await;

    match result {
        Err(join_err) => {
            tracing::error!(
                session_id = %session_id,
                "literature_context spawn_blocking panicked: {join_err}"
            );
            (StatusCode::INTERNAL_SERVER_ERROR, "internal error").into_response()
        }
        Ok(Ok(ctx)) => Json(ctx).into_response(),
        Ok(Err(LiteratureContextError::NoLiteratureAtoms)) => (
            StatusCode::NOT_FOUND,
            "no literature atoms ran in this session",
        )
            .into_response(),
        Ok(Err(LiteratureContextError::NoEmittedPackage)) => {
            // Shouldn't be reachable given the check above, but handle
            // defensively to avoid an unmatched arm.
            (StatusCode::CONFLICT, "package not yet emitted").into_response()
        }
        Ok(Err(LiteratureContextError::CsvSchemaMismatch(reason))) => {
            tracing::error!(
                session_id = %session_id,
                "literature CSV schema mismatch: {reason}"
            );
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "literature CSV schema mismatch — see server logs",
            )
                .into_response()
        }
    }
}

// ── Route inventory ───────────────────────────────────────────────────────────

pub(super) const ROUTES: &[(&str, &str)] = &[("GET", "/api/chat/session/:id/literature-context")];

pub(super) fn routes() -> axum::Router<ChatAppState> {
    axum::Router::new().route(
        "/api/chat/session/:id/literature-context",
        axum::routing::get(get_literature_context),
    )
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat_routes::test_support::{body_json, make_router};
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tempfile::TempDir;
    use tower::util::ServiceExt;

    fn write_minimal_package(dir: &std::path::Path, entity: &str, kind: &str, pmid: &str) {
        let task_dir = dir.join("runtime/outputs/review_prior_work");
        let evidence_dir = task_dir.join("evidence");
        std::fs::create_dir_all(&evidence_dir).unwrap();

        let csv_content = format!(
            "entity,entity_kind,pmid,evidence_quote,evidence_quote_offset,source_kind,source_hash,retrieval_ts,redistributable,verified\n\
             {entity},{kind},{pmid},quote,0,pmc_oa_full_text,sha256:abc,2026-05-14T00:00:00Z,true,true\n"
        );
        std::fs::write(task_dir.join("prior_claims_matrix.csv"), csv_content).unwrap();

        let manifest = serde_json::json!({
            "schema_version": 1,
            "entries": [{
                "pmid": pmid,
                "source_kind": "pmc_oa_full_text",
                "path": format!("{pmid}.xml"),
                "sha256_binary": "00",
                "sha256_extracted_text": "00",
                "extracted_text_normalization": "collapse_whitespace_lowercase_v1",
                "bytes": 0,
                "retrieval_ts": "2026-05-14T00:00:00Z",
                "retrieval_query_id": "q001",
                "redistributable": true,
                "license": "CC-BY-4.0"
            }]
        });
        std::fs::write(evidence_dir.join("manifest.json"), manifest.to_string()).unwrap();
    }

    // ── helpers ───────────────────────────────────────────────────────────

    /// Create a session and set emitted_package_path to `pkg_root`.
    async fn seed_emitted_session(
        app: &ChatAppState,
        pkg_root: Option<std::path::PathBuf>,
    ) -> Uuid {
        let (id, _) = app.conversation.start_session(false).await.unwrap();
        app.conversation
            .store_handle()
            .update(id, |s| {
                s.emitted_package_path = pkg_root;
                Ok(())
            })
            .await
            .unwrap();
        id
    }

    fn lit_req(session_id: Uuid, qs: &str) -> Request<Body> {
        Request::builder()
            .method("GET")
            .uri(format!(
                "/api/chat/session/{session_id}/literature-context?{qs}"
            ))
            .body(Body::empty())
            .unwrap()
    }

    // ── 404: session not found ────────────────────────────────────────────

    #[tokio::test]
    async fn returns_404_when_session_not_found() {
        let (router, _app) = make_router(vec![]).await;
        let fake_id = Uuid::new_v4();
        let resp = router
            .oneshot(lit_req(fake_id, "entity=ACAN"))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    // ── 409: package not yet emitted ─────────────────────────────────────

    #[tokio::test]
    async fn returns_409_when_package_not_yet_emitted() {
        let (router, app) = make_router(vec![]).await;
        let id = seed_emitted_session(&app, None).await;
        let resp = router.oneshot(lit_req(id, "entity=ACAN")).await.unwrap();
        assert_eq!(resp.status(), StatusCode::CONFLICT);
        let body = body_json(resp.into_body()).await;
        assert!(body.get("state").is_some(), "409 body should include state");
    }

    // ── 404: no literature atoms ran ──────────────────────────────────────

    #[tokio::test]
    async fn returns_404_when_no_literature_atoms_ran() {
        let pkg_dir = TempDir::new().unwrap();
        // Package dir exists but contains no CSV files → NoLiteratureAtoms.
        let pkg_root = pkg_dir.path().to_path_buf();

        let (router, app) = make_router(vec![]).await;
        let id = seed_emitted_session(&app, Some(pkg_root)).await;
        let resp = router.oneshot(lit_req(id, "entity=ACAN")).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        std::mem::forget(pkg_dir);
    }

    // ── 200: empty rows when entity not present ───────────────────────────

    #[tokio::test]
    async fn returns_200_with_empty_rows_when_entity_not_present() {
        let pkg_dir = TempDir::new().unwrap();
        write_minimal_package(pkg_dir.path(), "ACAN", "gene", "28123456");
        let pkg_root = pkg_dir.path().to_path_buf();

        let (router, app) = make_router(vec![]).await;
        let id = seed_emitted_session(&app, Some(pkg_root)).await;
        let resp = router
            .oneshot(lit_req(id, "entity=UNKNOWN_GENE"))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp.into_body()).await;
        assert_eq!(body["prior_rows"].as_array().unwrap().len(), 0);
        std::mem::forget(pkg_dir);
    }

    // ── 200: matching rows returned ───────────────────────────────────────

    #[tokio::test]
    async fn returns_200_with_matching_rows_when_present() {
        let pkg_dir = TempDir::new().unwrap();
        write_minimal_package(pkg_dir.path(), "ACAN", "gene", "28123456");
        let pkg_root = pkg_dir.path().to_path_buf();

        let (router, app) = make_router(vec![]).await;
        let id = seed_emitted_session(&app, Some(pkg_root)).await;
        let resp = router
            .oneshot(lit_req(id, "entity=ACAN&entity_kind=gene"))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp.into_body()).await;
        let rows = body["prior_rows"].as_array().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["pmid"], "28123456");
        assert_eq!(body["source_artifacts"].as_array().unwrap().len(), 1);
        std::mem::forget(pkg_dir);
    }
}
