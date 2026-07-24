//! Read-only fetch of the two multi-analyst-ensemble runtime artifacts:
//! `ensemble-distribution.json` (produced by `assemble_ensemble_distribution`)
//! and `stat-distribution.json` (produced by `assemble_statistical_distribution`).
//! Both aggregators hardcode their output dir to the bare stage id, so the
//! artifact paths below are fixed — no user-controlled path segment, and
//! therefore no path-jail is needed (same reasoning as
//! `verification::get_cross_version_diff`). Mirrors that handler's shape
//! exactly, including the capped-read helper.

use super::*;
use axum::{extract::State, http::StatusCode, response::IntoResponse, Json};
use tokio::io::AsyncReadExt;
use uuid::Uuid;

// NOTE: `super::*` glob-imports `axum::extract::Path` (the extractor used
// by the handlers below). Filesystem paths in this module therefore use
// the fully-qualified `std::path::Path`/`std::path::PathBuf` rather than
// a local `use std::path::Path`, which would shadow the extractor and
// break `axum::routing::get(handler)`'s `Handler` trait resolution.
use std::path::Path as FsPath;

/// Package-relative path to the cross-analyst distribution summary.
const ENSEMBLE_DIST_REL: &str =
    "runtime/outputs/assemble_ensemble_distribution/ensemble-distribution.json";
/// Package-relative path to the statistical-ensemble distribution summary.
const STAT_DIST_REL: &str =
    "runtime/outputs/assemble_statistical_distribution/stat-distribution.json";

/// Cap on bytes read into memory when slurping an ensemble sidecar. See
/// `verification::SIDECAR_READ_CAP_BYTES` for the identical rationale —
/// bounds worst-case allocation from a malformed or runaway artifact.
const SIDECAR_READ_CAP_BYTES: u64 = 16 * 1024 * 1024;

/// Open `path`, read at most `SIDECAR_READ_CAP_BYTES` into memory, and
/// parse as JSON. Returns `Ok(None)` on missing file (treat as 404);
/// `Ok(Some(v))` on successful parse; `Err(_)` on I/O or deserialisation
/// error. Copy of `verification::read_capped_json` — kept local rather
/// than shared so each submodule's artifact-read contract can evolve
/// independently.
async fn read_capped_json(path: &FsPath) -> std::io::Result<Option<serde_json::Value>> {
    let file = match tokio::fs::File::open(path).await {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    let mut buf = Vec::new();
    file.take(SIDECAR_READ_CAP_BYTES)
        .read_to_end(&mut buf)
        .await?;
    let v = serde_json::from_slice::<serde_json::Value>(&buf)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;
    Ok(Some(v))
}

/// Join a fixed, package-relative path onto `package_root`. No
/// user-controlled segment ever reaches this function — both ensemble
/// aggregators hardcode their output dir to the bare stage id (single-
/// AND multi-branch runs), so the fixed relative path resolves in both
/// topologies without needing per-branch disambiguation.
fn ensemble_artifact_path(package_root: &FsPath, rel: &str) -> std::path::PathBuf {
    let mut p = package_root.to_path_buf();
    for seg in rel.split('/') {
        p.push(seg);
    }
    p
}

/// Resolve + read the ensemble-distribution artifact under `package_root`.
async fn resolve_ensemble_distribution(
    package_root: &FsPath,
) -> std::io::Result<Option<serde_json::Value>> {
    read_capped_json(&ensemble_artifact_path(package_root, ENSEMBLE_DIST_REL)).await
}

/// Resolve + read the stat-distribution artifact under `package_root`.
async fn resolve_stat_distribution(
    package_root: &FsPath,
) -> std::io::Result<Option<serde_json::Value>> {
    read_capped_json(&ensemble_artifact_path(package_root, STAT_DIST_REL)).await
}

/// Surface the cross-analyst ensemble-distribution summary for this
/// session's latest emit. 404 (canonical `ApiError::NotFound`) when the
/// session hasn't emitted a package, or when no ensemble ran for it —
/// the UI's Robustness tab treats 404 as "no ensemble data yet" rather
/// than a fetch error.
#[tracing::instrument(skip(app), fields(session_id = %session_id))]
pub async fn get_ensemble_distribution(
    State(app): State<ChatAppState>,
    Path(session_id): Path<Uuid>,
) -> impl IntoResponse {
    let Some(session) = app.conversation.get_session(session_id).await else {
        return crate::error::ApiError::NotFound("session not found".into()).into_response();
    };
    let Some(root) = session.emitted_package_path.clone() else {
        return crate::error::ApiError::NotFound(
            "ensemble distribution not available (package not yet emitted)".into(),
        )
        .into_response();
    };
    match resolve_ensemble_distribution(&root).await {
        Ok(Some(v)) => Json(v).into_response(),
        Ok(None) => crate::error::ApiError::NotFound(
            "ensemble distribution not produced for this session".into(),
        )
        .into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

/// Surface the statistical-ensemble distribution summary for this
/// session's latest emit. Same absence semantics as
/// [`get_ensemble_distribution`].
#[tracing::instrument(skip(app), fields(session_id = %session_id))]
pub async fn get_stat_distribution(
    State(app): State<ChatAppState>,
    Path(session_id): Path<Uuid>,
) -> impl IntoResponse {
    let Some(session) = app.conversation.get_session(session_id).await else {
        return crate::error::ApiError::NotFound("session not found".into()).into_response();
    };
    let Some(root) = session.emitted_package_path.clone() else {
        return crate::error::ApiError::NotFound(
            "stat distribution not available (package not yet emitted)".into(),
        )
        .into_response();
    };
    match resolve_stat_distribution(&root).await {
        Ok(Some(v)) => Json(v).into_response(),
        Ok(None) => crate::error::ApiError::NotFound(
            "stat distribution not produced for this session".into(),
        )
        .into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

/// Route inventory for the doc-as-contract gate + per-submodule
/// `routes()` builder. `mod.rs::router()` merges every submodule's
/// builder into the single chat surface.
pub(super) const ROUTES: &[(&str, &str)] = &[
    ("GET", "/api/chat/session/:id/ensemble-distribution"),
    ("GET", "/api/chat/session/:id/stat-distribution"),
];

pub(super) fn routes() -> axum::Router<ChatAppState> {
    axum::Router::new()
        .route(
            "/api/chat/session/:id/ensemble-distribution",
            axum::routing::get(get_ensemble_distribution),
        )
        .route(
            "/api/chat/session/:id/stat-distribution",
            axum::routing::get(get_stat_distribution),
        )
}

#[cfg(test)]
mod tests {
    use super::{resolve_ensemble_distribution, resolve_stat_distribution};
    use crate::chat_routes::test_support::make_router;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::util::ServiceExt;

    // ── Path-resolver unit tests ────────────────────────────────────────
    //
    // Written before the resolvers existed (TDD step 1): a temp package
    // dir holds both artifacts under their fixed runtime/outputs paths;
    // the resolvers must return the parsed content when present, and
    // `Ok(None)` when a package root lacks the file entirely.

    #[tokio::test]
    async fn resolves_ensemble_distribution_when_present() {
        let pkg = tempfile::TempDir::new().unwrap();
        let dir = pkg
            .path()
            .join("runtime/outputs/assemble_ensemble_distribution");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("ensemble-distribution.json"),
            r#"{"n_analysts": 5, "consensus": "up"}"#,
        )
        .unwrap();

        let got = resolve_ensemble_distribution(pkg.path()).await.unwrap();
        assert_eq!(
            got,
            Some(serde_json::json!({"n_analysts": 5, "consensus": "up"}))
        );
    }

    #[tokio::test]
    async fn resolves_stat_distribution_when_present() {
        let pkg = tempfile::TempDir::new().unwrap();
        let dir = pkg
            .path()
            .join("runtime/outputs/assemble_statistical_distribution");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("stat-distribution.json"),
            r#"{"n_bootstraps": 1000, "ci_width": 0.12}"#,
        )
        .unwrap();

        let got = resolve_stat_distribution(pkg.path()).await.unwrap();
        assert_eq!(
            got,
            Some(serde_json::json!({"n_bootstraps": 1000, "ci_width": 0.12}))
        );
    }

    #[tokio::test]
    async fn resolvers_return_none_when_artifact_absent() {
        let pkg = tempfile::TempDir::new().unwrap();
        assert_eq!(
            resolve_ensemble_distribution(pkg.path()).await.unwrap(),
            None
        );
        assert_eq!(resolve_stat_distribution(pkg.path()).await.unwrap(), None);
    }

    // ── HTTP-level route tests ───────────────────────────────────────────

    #[tokio::test]
    async fn ensemble_distribution_route_404_without_emitted_package() {
        let (router, app) = make_router(vec![]).await;
        let (session_id, _) = app.conversation.start_session(false).await.unwrap();
        let req = Request::builder()
            .uri(format!(
                "/api/chat/session/{}/ensemble-distribution",
                session_id
            ))
            .body(Body::empty())
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn stat_distribution_route_200_with_emitted_package() {
        let (router, app) = make_router(vec![]).await;
        let (session_id, _) = app.conversation.start_session(false).await.unwrap();

        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp
            .path()
            .join("runtime/outputs/assemble_statistical_distribution");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("stat-distribution.json"), r#"{"ok": true}"#).unwrap();
        app.conversation
            .store_handle()
            .update(session_id, |s| {
                s.emitted_package_path = Some(tmp.path().to_path_buf());
                Ok(())
            })
            .await
            .unwrap();

        let req = Request::builder()
            .uri(format!(
                "/api/chat/session/{}/stat-distribution",
                session_id
            ))
            .body(Body::empty())
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = crate::chat_routes::test_support::body_json(resp.into_body()).await;
        assert_eq!(body, serde_json::json!({"ok": true}));
    }
}
