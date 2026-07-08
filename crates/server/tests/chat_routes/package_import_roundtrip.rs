//! Round-trip integration test for the package-import surface: POST an
//! in-memory ECAA `.zip`, assert a read-only imported session is created and
//! explorable, and assert lifecycle mutations + Tier-2 replay are refused.
//!
//! The router is built like the other `tests/chat_routes/*` integration
//! tests (`v1_api_mount.rs`): `ChatAppState::with_backend` + a test-default
//! `RequestPrincipal` extension. `with_backend` uses `Config::for_test()`, so
//! `app.config.package_root` lands under `/tmp/ecaa-workflow-test-default`;
//! each import writes to a per-request UUID subdir so parallel tests don't
//! collide.

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
/// dir (mirrors the exporter's `<basename>/` nesting). Minimal-audit tier:
/// no scripts / result tables / determinism-env → `replay_tier2 == false`.
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
        zw.start_file("pkg/runtime/audit-proof-report.json", o).unwrap();
        zw.write_all(b"{}").unwrap();
        zw.start_file("pkg/runtime/claim-verification.json", o).unwrap();
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

#[tokio::test]
async fn import_zip_creates_explorable_session() {
    let router = router().await;
    let zip = tiny_package_zip();
    let req = Request::builder()
        .method("POST")
        .uri("/api/chat/package/import")
        .header("content-type", "application/zip")
        .body(Body::from(zip))
        .unwrap();
    let resp = router.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body: serde_json::Value = {
        let b = to_bytes(resp.into_body(), 1_000_000).await.unwrap();
        serde_json::from_slice(&b).unwrap()
    };
    let sid = body["session_id"].as_str().unwrap();
    assert_eq!(body["imported"], serde_json::json!(true));
    assert_eq!(body["capabilities"]["explore"], serde_json::json!(true));
    assert_eq!(body["capabilities"]["replay_tier2"], serde_json::json!(false));

    // Explorable: /dag returns the reconstructed graph.
    let dag_req = Request::builder()
        .method("GET")
        .uri(format!("/api/chat/session/{sid}/dag"))
        .body(Body::empty())
        .unwrap();
    let dag_resp = router.oneshot(dag_req).await.unwrap();
    assert_eq!(dag_resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn import_rejects_non_ecaa_zip() {
    let router = router().await;
    let mut buf = std::io::Cursor::new(Vec::new());
    {
        let mut zw = zip::ZipWriter::new(&mut buf);
        let o = zip::write::SimpleFileOptions::default();
        zw.start_file("notes.txt", o).unwrap();
        zw.write_all(b"hi").unwrap();
        zw.finish().unwrap();
    }
    let req = Request::builder()
        .method("POST")
        .uri("/api/chat/package/import")
        .header("content-type", "application/zip")
        .body(Body::from(buf.into_inner()))
        .unwrap();
    let resp = router.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn imported_session_refuses_execution_and_tier2() {
    let router = router().await;
    // Import first.
    let req = Request::builder()
        .method("POST")
        .uri("/api/chat/package/import")
        .header("content-type", "application/zip")
        .body(Body::from(tiny_package_zip()))
        .unwrap();
    let resp = router.clone().oneshot(req).await.unwrap();
    let body: serde_json::Value = {
        let b = to_bytes(resp.into_body(), 1_000_000).await.unwrap();
        serde_json::from_slice(&b).unwrap()
    };
    let sid = body["session_id"].as_str().unwrap().to_string();

    // start-execution refused with 412 (read-only imported package).
    let ex = Request::builder()
        .method("POST")
        .uri(format!("/api/chat/session/{sid}/start-execution"))
        .header("content-type", "application/json")
        .body(Body::from("{}"))
        .unwrap();
    let ex_resp = router.clone().oneshot(ex).await.unwrap();
    assert_eq!(ex_resp.status(), StatusCode::PRECONDITION_FAILED);

    // Tier-2 replay refused with 412 (minimal package → not re-executable).
    let rp = Request::builder()
        .method("POST")
        .uri(format!("/api/chat/session/{sid}/replay"))
        .header("content-type", "application/json")
        .body(Body::from(r#"{"tier":"all"}"#))
        .unwrap();
    let rp_resp = router.oneshot(rp).await.unwrap();
    assert_eq!(rp_resp.status(), StatusCode::PRECONDITION_FAILED);
}
