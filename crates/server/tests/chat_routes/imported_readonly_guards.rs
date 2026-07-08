//! Read-only enforcement for imported ECAA packages. An imported session
//! is strictly read-only: no execution, no mutation. An adversarial audit
//! found several task-scoped endpoints that executed or mutated an imported
//! session; this test pins the `ensure_not_imported` guard (412
//! PRECONDITION_FAILED) on the endpoints that were missing it.
//!
//! Router shape mirrors `package_import_roundtrip.rs`:
//! `ChatAppState::with_backend` + a test-default `RequestPrincipal`
//! extension. Each import writes to a per-request UUID subdir under
//! `Config::for_test()`'s package_root so parallel tests don't collide.

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use ecaa_workflow_conversation::{LlmBackend, MockLlmBackend, SessionStore};
use ecaa_workflow_server::chat_routes;
use std::io::Write;
use std::path::Path;
use std::sync::Arc;
use tower::util::ServiceExt;

fn config_dir() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("config")
}

/// A minimal but valid ECAA package nested under a single top-level `pkg/`
/// dir. One completed `data_acq` task using the snake_case WORKFLOW.json
/// shape the reconstructor accepts.
fn tiny_package_zip() -> Vec<u8> {
    let dag = serde_json::json!({
        "version":"1","workflow_id":"wf","current_task":null,
        "tasks":{"data_acq":{"kind":{"discovery":"source"},"state":{"status":"completed","result":{}},"depends_on":[],"assignee":"agent","description":"x"}},
        "execution_order":["data_acq"]
    });
    let mut buf = std::io::Cursor::new(Vec::new());
    {
        let mut zw = zip::ZipWriter::new(&mut buf);
        let o = zip::write::SimpleFileOptions::default();
        zw.start_file("pkg/WORKFLOW.json", o).unwrap();
        zw.write_all(serde_json::to_vec_pretty(&dag).unwrap().as_slice())
            .unwrap();
        zw.start_file("pkg/ro-crate-metadata.json", o).unwrap();
        zw.write_all(b"{}").unwrap();
        zw.finish().unwrap();
    }
    buf.into_inner()
}

async fn router() -> axum::Router {
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::open(dir.path()).await.unwrap();
    std::mem::forget(dir); // keep the sessions temp dir alive for the test
    let backend: Arc<dyn LlmBackend> = Arc::new(MockLlmBackend::new(vec![]));
    let app = chat_routes::ChatAppState::with_backend(backend, store, config_dir());
    chat_routes::router(app).layer(axum::Extension(
        ecaa_workflow_server::auth::RequestPrincipal::test_default(),
    ))
}

/// Import the tiny package and return the created (read-only) session id.
async fn import_session(router: &axum::Router) -> String {
    let req = Request::builder()
        .method("POST")
        .uri("/api/chat/package/import")
        .header("content-type", "application/zip")
        .body(Body::from(tiny_package_zip()))
        .unwrap();
    let resp = router.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "import must succeed");
    let body: serde_json::Value = {
        let b = to_bytes(resp.into_body(), 1_000_000).await.unwrap();
        serde_json::from_slice(&b).unwrap()
    };
    assert_eq!(body["imported"], serde_json::json!(true));
    body["session_id"].as_str().unwrap().to_string()
}

async fn post(router: &axum::Router, uri: String, body: &'static str) -> StatusCode {
    let req = Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/json")
        .body(Body::from(body))
        .unwrap();
    router.clone().oneshot(req).await.unwrap().status()
}

/// The four endpoints that executed / mutated an imported session before
/// this remediation must all refuse with 412 PRECONDITION_FAILED.
#[tokio::test]
async fn imported_session_refuses_execution_and_mutation_endpoints() {
    let router = router().await;
    let sid = import_session(&router).await;

    // rerun-script — ran bare host `bash` on package scripts.
    assert_eq!(
        post(
            &router,
            format!("/api/chat/session/{sid}/task/data_acq/rerun-script"),
            r#"{"rel_path":"x.sh"}"#,
        )
        .await,
        StatusCode::PRECONDITION_FAILED,
        "rerun-script must be refused on an imported session"
    );

    // sme-selection — writes selection sidecars + resumes + auto-relaunches.
    assert_eq!(
        post(
            &router,
            format!("/api/chat/session/{sid}/task/data_acq/sme-selection"),
            r#"{"chosen":"x"}"#,
        )
        .await,
        StatusCode::PRECONDITION_FAILED,
        "sme-selection must be refused on an imported session"
    );

    // state — mutates task_states.
    assert_eq!(
        post(
            &router,
            format!("/api/chat/session/{sid}/task/data_acq/state"),
            r#"{"state":{"status":"ready"}}"#,
        )
        .await,
        StatusCode::PRECONDITION_FAILED,
        "task/state must be refused on an imported session"
    );

    // auto-approve-discoveries — writes the auto-approve sentinel file.
    assert_eq!(
        post(
            &router,
            format!("/api/chat/session/{sid}/auto-approve-discoveries"),
            "{}",
        )
        .await,
        StatusCode::PRECONDITION_FAILED,
        "auto-approve-discoveries must be refused on an imported session"
    );
}
