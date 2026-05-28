//! Atom-safety-policy POST endpoint that widens an atom's
//! `runtime_packages` set from the BlockerCard's `ProvisioningDenied`
//! affordance.
//!
//! Endpoint: `POST /api/chat/session/:id/atom/:atom_id/add-runtime-package`
//! Body: `{"package": "samtools", "registry": "apt"}`
//! Returns: 204 No Content on success; 400 on validation failure;
//! 404 when the session is missing.
//!
//! Behavior:
//! 1. Path-jail (RC-17): `atom_id` is validated as a single safe
//!    segment via `safe_segment_join` even though it isn't used as a
//!    filesystem path directly — the same defensive contract every
//!    URL-path component on the chat surface follows.
//! 2. Delegates to `ConversationService::add_runtime_package_from_rest`,
//!    which is the single mutation site that:
//! - Inserts into `Session::atom_runtime_overrides` (idempotent
//!   BTreeSet semantics).
//! - In-place patches `policies/runtime-prereqs.json` in the
//!   emitted package so the harness install-proxy reads the
//!   widened set on next dispatch.
//! - Records a `DecisionType::RuntimePackageAdded` audit entry.
//! 3. Fires the git-commit hook through the bounded
//!    `app.git_hook_pool` (RC-20) so the per-package git history
//!    captures the override.
//!
//! No state transition: the SME is widening an installer allowlist,
//! not amending the workflow plan, so the session stays in whatever
//! state it was in (typically `Blocked` with `ProvisioningDenied`).
//! The SME's next click after this is typically Unblock + Rerun,
//! which fires the existing transition endpoints.

use super::super::{ChatAppState, DropNotifier, SsePayload};
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use serde::Deserialize;
use std::sync::Arc;
use uuid::Uuid;

#[derive(Debug, Deserialize)]
pub(crate) struct AddRuntimePackageRequest {
    /// Package the SME approved. Must be non-empty after trim.
    /// Format varies per registry: `samtools` for apt, `scanpy>=1.10`
    /// for pip, `Seurat>=5.0` for cran, etc.
    pub package: String,
    /// Package manager / registry the package is installed from.
    /// One of `apt`, `dnf`, `pip` (or `pypi`), `cran` (or `r`),
    /// `conda`. Unknown registries return 400.
    pub registry: String,
}

/// REST counterpart of the BlockerCard's "Add `<package>` to
/// atom.runtime_packages" button. Surfaces validation failures from
/// the service layer (empty package, unknown registry) as 400. A
/// missing session surfaces as 404 (vs amend-method, which returns
/// 400 with a stripped prefix — but amend's missing-session path
/// produces an `Internal` error too; the difference is only in the
/// HTTP code, and ProvisioningDenied is a session-scoped concept so
/// 404 matches the SME's mental model).
pub(crate) async fn post_add_runtime_package(
    State(app): State<ChatAppState>,
    Path((session_id, atom_id)): Path<(Uuid, String)>,
    Json(req): Json<AddRuntimePackageRequest>,
) -> impl IntoResponse {
    // Path-jail: the atom_id is a URL-path component spliced into
    // session state; validate it's a single safe segment. Even though
    // we don't write it to disk directly, applying the same path-jail
    // contract keeps every chat-surface component on the same
    // defensive footing.
    //
    // The "root" we jail against is purely synthetic — the helper
    // only checks shape (no `..`, no `/` or `\`, non-empty,
    // non-absolute), so any path works as the anchor. We use a fixed
    // sentinel so the rejection message is clear when the SME tries
    // anything weird.
    if let Err(e) = super::super::safe_segment_join(std::path::Path::new("/atoms"), &atom_id) {
        return (StatusCode::BAD_REQUEST, format!("invalid atom_id: {}", e)).into_response();
    }
    // Surface the missing-session case as 404 explicitly (the service
    // layer would surface it as `Internal` otherwise). The
    // session-existence check is cheap (Arc clone of an Arc<Session>
    // pulled from the in-memory store).
    if app.conversation.get_session(session_id).await.is_none() {
        return (StatusCode::NOT_FOUND, "session not found").into_response();
    }
    match app
        .conversation
        .add_runtime_package_from_rest(
            session_id,
            atom_id.clone(),
            req.package.clone(),
            req.registry.clone(),
        )
        .await
    {
        Ok(()) => {
            // Fire the git-commit hook through the bounded pool.
            // Resolve the session's emitted_package_path so the hook
            // commits into the right per-package.git directory;
            // missing package path means the SME widened the catalog
            // before the first emit — no on-disk history to commit,
            // and the in-memory override carries through to the first
            // emit which will fire its own commit hook.
            if let Some(session) = app.conversation.get_session(session_id).await {
                if let Some(pkg) = session.emitted_package_path.clone() {
                    let cfg = app.git_config().read().clone();
                    let sid = session_id.to_string();
                    let atom = atom_id.clone();
                    let package = req.package.clone();
                    let app_for_drop = app.clone();
                    let drop_notifier: DropNotifier =
                        Arc::new(move |trigger: &str, reason: &str| {
                            app_for_drop.spawn_fanout(
                                session_id,
                                SsePayload::ProvenanceCommitDropped {
                                    trigger: trigger.to_string(),
                                    reason: reason.to_string(),
                                },
                            );
                        });
                    app.git_hook_pool.spawn_with_sink(
                        "add_runtime_package",
                        move || {
                            crate::git_routes::service::hook_commit(
                                &cfg,
                                &pkg,
                                "add_runtime_package",
                                &format!("{} runtime_packages += {}", atom, package),
                                &sid,
                            );
                            Ok(())
                        },
                        Some(drop_notifier),
                    );
                }
            }
            StatusCode::NO_CONTENT.into_response()
        }
        Err(e) => {
            // Strip the ServiceError::Internal prefix so the client
            // sees the validation reason ("package is required",
            // "unknown registry …") rather than a generic "internal
            // error: …" wrapper. Mirrors the amend-method handler.
            let msg = format!("{}", e);
            let cleaned = msg
                .strip_prefix("internal error: ")
                .unwrap_or(&msg)
                .to_string();
            (StatusCode::BAD_REQUEST, cleaned).into_response()
        }
    }
}

pub(crate) fn routes() -> axum::Router<ChatAppState> {
    axum::Router::new().route(
        "/api/chat/session/:id/atom/:atom_id/add-runtime-package",
        axum::routing::post(post_add_runtime_package),
    )
}

#[cfg(test)]
mod tests {
    use crate::chat_routes::test_support::make_router;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::util::ServiceExt;

    /// Happy path: POST with a valid body returns 204 and persists the
    /// override on the session.
    #[tokio::test]
    async fn add_runtime_package_returns_204_on_success() {
        let (router, app) = make_router(vec![]).await;
        let (id, _) = app.conversation.start_session(false).await.unwrap();
        let req = Request::builder()
            .method("POST")
            .uri(format!(
                "/api/chat/session/{}/atom/align_reads/add-runtime-package",
                id
            ))
            .header("content-type", "application/json")
            .body(Body::from(r#"{"package":"samtools","registry":"apt"}"#))
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::NO_CONTENT,
            "happy path should return 204"
        );

        // Side-effect 1: session field carries the override.
        let session = app.conversation.get_session(id).await.unwrap();
        let entry = session
            .atom_runtime_overrides
            .get("align_reads")
            .expect("atom entry should be present");
        let pkgs = entry.get("apt").expect("apt registry should be present");
        assert!(pkgs.contains("samtools"), "package should be inserted");

        // Side-effect 2: decision was recorded.
        let has_decision = session.decisions.iter().any(|r| matches!(
            &r.decision,
            scripps_workflow_core::decision_log::DecisionType::RuntimePackageAdded { atom_id, package, registry }
                if atom_id.as_str() == "align_reads" && package == "samtools" && registry == "apt"
        ));
        assert!(
            has_decision,
            "decisions log should carry the RuntimePackageAdded entry"
        );
    }

    /// Idempotency: posting the same package twice produces one entry
    /// in the override set but two decision records (so the SME's
    /// clickstream is fully captured).
    #[tokio::test]
    async fn add_runtime_package_is_idempotent_on_override_set() {
        let (router, app) = make_router(vec![]).await;
        let (id, _) = app.conversation.start_session(false).await.unwrap();
        let body = r#"{"package":"samtools","registry":"apt"}"#;

        for _ in 0..2 {
            let req = Request::builder()
                .method("POST")
                .uri(format!(
                    "/api/chat/session/{}/atom/align_reads/add-runtime-package",
                    id
                ))
                .header("content-type", "application/json")
                .body(Body::from(body))
                .unwrap();
            let resp = router.clone().oneshot(req).await.unwrap();
            assert_eq!(resp.status(), StatusCode::NO_CONTENT);
        }

        let session = app.conversation.get_session(id).await.unwrap();
        let pkgs = session
            .atom_runtime_overrides
            .get("align_reads")
            .and_then(|m| m.get("apt"))
            .expect("override entry should exist");
        assert_eq!(
            pkgs.len(),
            1,
            "duplicate add should not grow the BTreeSet (got {:?})",
            pkgs
        );
        let runtime_adds = session
            .decisions
            .iter()
            .filter(|r| {
                matches!(
                    &r.decision,
                    scripps_workflow_core::decision_log::DecisionType::RuntimePackageAdded { .. }
                )
            })
            .count();
        assert_eq!(
            runtime_adds, 2,
            "each click should write a decision record (got {})",
            runtime_adds
        );
    }

    /// Validation: empty package surfaces as 400 with a useful
    /// message.
    #[tokio::test]
    async fn add_runtime_package_400_on_empty_package() {
        let (router, app) = make_router(vec![]).await;
        let (id, _) = app.conversation.start_session(false).await.unwrap();
        let req = Request::builder()
            .method("POST")
            .uri(format!(
                "/api/chat/session/{}/atom/align_reads/add-runtime-package",
                id
            ))
            .header("content-type", "application/json")
            .body(Body::from(r#"{"package":"   ","registry":"apt"}"#))
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    /// Validation: unknown registry surfaces as 400.
    #[tokio::test]
    async fn add_runtime_package_400_on_unknown_registry() {
        let (router, app) = make_router(vec![]).await;
        let (id, _) = app.conversation.start_session(false).await.unwrap();
        let req = Request::builder()
            .method("POST")
            .uri(format!(
                "/api/chat/session/{}/atom/align_reads/add-runtime-package",
                id
            ))
            .header("content-type", "application/json")
            .body(Body::from(
                r#"{"package":"samtools","registry":"my-custom-yum"}"#,
            ))
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    /// 404 on missing session — the SME has no business widening a
    /// catalog for a session that doesn't exist.
    #[tokio::test]
    async fn add_runtime_package_404_on_missing_session() {
        let (router, _) = make_router(vec![]).await;
        let fake = uuid::Uuid::new_v4();
        let req = Request::builder()
            .method("POST")
            .uri(format!(
                "/api/chat/session/{}/atom/align_reads/add-runtime-package",
                fake
            ))
            .header("content-type", "application/json")
            .body(Body::from(r#"{"package":"samtools","registry":"apt"}"#))
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    /// An atom_id with a separator should be rejected by the
    /// path-jail. The URL parser would normally normalize a literal
    /// `/`, so we exercise the URL-encoded form `%2F` to thread it
    /// through the Path extractor verbatim. The handler's
    /// `safe_segment_join` check rejects it before we touch the
    /// service layer.
    #[tokio::test]
    async fn add_runtime_package_rejects_atom_id_with_traversal() {
        let (router, app) = make_router(vec![]).await;
        let (id, _) = app.conversation.start_session(false).await.unwrap();
        // axum's Path extractor decodes `%2F` to `/`, which means the
        // atom_id segment carries an embedded separator. Our path-jail
        // helper rejects this on Segregator.
        let req = Request::builder()
            .method("POST")
            .uri(format!(
                "/api/chat/session/{}/atom/foo%2Fbar/add-runtime-package",
                id
            ))
            .header("content-type", "application/json")
            .body(Body::from(r#"{"package":"samtools","registry":"apt"}"#))
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::BAD_REQUEST,
            "atom_id with embedded separator must be rejected"
        );
    }

    /// Registry alias normalization: `pypi` collapses onto the `pip`
    /// key so the BTreeSet stays single-shaped.
    #[tokio::test]
    async fn add_runtime_package_normalizes_pypi_to_pip() {
        let (router, app) = make_router(vec![]).await;
        let (id, _) = app.conversation.start_session(false).await.unwrap();
        let req = Request::builder()
            .method("POST")
            .uri(format!(
                "/api/chat/session/{}/atom/qc/add-runtime-package",
                id
            ))
            .header("content-type", "application/json")
            .body(Body::from(
                r#"{"package":"scanpy>=1.10","registry":"pypi"}"#,
            ))
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);
        let session = app.conversation.get_session(id).await.unwrap();
        let entry = session.atom_runtime_overrides.get("qc").unwrap();
        assert!(
            entry.contains_key("pip"),
            "pypi must normalize to pip; got keys {:?}",
            entry.keys().collect::<Vec<_>>()
        );
        assert!(!entry.contains_key("pypi"));
    }

    /// When the session has an `emitted_package_path`, the handler
    /// in-place patches `policies/runtime-prereqs.json` so the harness
    /// install-proxy sees the widened package on retry without
    /// waiting for a full re-emission.
    #[tokio::test]
    async fn add_runtime_package_patches_runtime_prereqs_file() {
        let (router, app) = make_router(vec![]).await;
        let tmp = tempfile::tempdir().unwrap();
        let pkg = tmp.path().to_path_buf();
        std::fs::create_dir_all(pkg.join("policies")).unwrap();
        // Seed an existing manifest so we can prove the patch
        // merges rather than overwrites.
        let mut existing = scripps_workflow_core::runtime_prereqs::RuntimePrereqs::new();
        existing.system_packages.apt.insert("git".into());
        std::fs::write(
            pkg.join("policies").join("runtime-prereqs.json"),
            serde_json::to_string_pretty(&existing).unwrap(),
        )
        .unwrap();

        let (id, _) = app.conversation.start_session(false).await.unwrap();
        app.conversation
            .store_handle()
            .update(id, |s| {
                s.emitted_package_path = Some(pkg.clone());
                Ok(())
            })
            .await
            .unwrap();

        let req = Request::builder()
            .method("POST")
            .uri(format!(
                "/api/chat/session/{}/atom/align_reads/add-runtime-package",
                id
            ))
            .header("content-type", "application/json")
            .body(Body::from(r#"{"package":"samtools","registry":"apt"}"#))
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);

        let bytes = std::fs::read(pkg.join("policies").join("runtime-prereqs.json")).unwrap();
        let patched: scripps_workflow_core::runtime_prereqs::RuntimePrereqs =
            serde_json::from_slice(&bytes).unwrap();
        assert!(
            patched.system_packages.apt.contains("samtools"),
            "newly added package must be in the file"
        );
        assert!(
            patched.system_packages.apt.contains("git"),
            "existing package must be preserved (merge, not overwrite)"
        );
    }
}
