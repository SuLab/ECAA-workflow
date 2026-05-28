//! Serve `config/stage-descriptions.yaml` as JSON to the UI. Pure
//! read-only; the file is session-agnostic (same config for every
//! session on this server).
//!
//! The YAML is parsed once per request and mapped into a flat
//! `{ <stage_class>: StageDescription }` shape + a `default` key that
//! the UI falls back to when it hits an unknown stage class.

use super::ChatAppState;
use axum::{http::StatusCode, response::IntoResponse, Json};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct StageDescription {
    pub sme_friendly_name: String,
    pub short: String,
    #[serde(default)]
    pub long: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub example_inputs: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub example_outputs: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct StageDescriptionsFile {
    #[serde(default)]
    version: u32,
    stages: std::collections::BTreeMap<String, StageDescription>,
}

#[derive(Debug, Serialize)]
struct StageDescriptionsResponse {
    version: u32,
    stages: std::collections::BTreeMap<String, StageDescription>,
}

fn config_path() -> PathBuf {
    let dir = std::env::var("SWFC_CONFIG_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("config"));
    dir.join("stage-descriptions.yaml")
}

pub(super) async fn get_stage_descriptions() -> impl IntoResponse {
    let path = config_path();
    let raw = match tokio::fs::read_to_string(&path).await {
        Ok(s) => s,
        Err(_) => {
            // Missing file: return empty descriptions rather than 500.
            // UI falls back to `task.description` from the compiled DAG.
            return Json(StageDescriptionsResponse {
                version: 0,
                stages: Default::default(),
            })
            .into_response();
        }
    };
    match serde_yml::from_str::<StageDescriptionsFile>(&raw) {
        Ok(parsed) => Json(StageDescriptionsResponse {
            version: parsed.version,
            stages: parsed.stages,
        })
        .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("failed to parse stage-descriptions.yaml: {}", e),
        )
            .into_response(),
    }
}

/// Route inventory for the doc-as-contract gate +
/// per-submodule `routes()` builder. `mod.rs::router()` merges every
/// submodule's builder into the single chat surface.
pub(super) const ROUTES: &[(&str, &str)] = &[("GET", "/api/chat/stage-descriptions")];

pub(super) fn routes() -> axum::Router<ChatAppState> {
    axum::Router::new().route(
        "/api/chat/stage-descriptions",
        axum::routing::get(get_stage_descriptions),
    )
}

#[cfg(test)]
mod tests {
    // S5.32: workspace lint is `unsafe_code = "deny"`. Test code uses
    // `unsafe { std::env::set_var / remove_var }` (unsafe in Rust 2024
    // edition because the env table is not thread-safe). All call sites
    // are single-threaded test setup/teardown; the bounded waiver is
    // scoped to this `mod tests` block.
    #![allow(unsafe_code)]
    use super::*;
    use axum::body::{to_bytes, Body};
    use axum::http::Request;
    use axum::routing::get;
    use axum::Router;
    use tower::util::ServiceExt;

    // Serialize every test in this module — they all mutate SWFC_CONFIG_DIR
    // and would race under parallel `cargo test`. tokio::sync::Mutex (vs
    // std::sync::Mutex) is async-aware, so holding the guard across the
    // test's `.await` calls doesn't trigger `clippy::await_holding_lock`.
    static ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    struct EnvGuard(Option<String>);
    impl EnvGuard {
        fn capture_and_set(value: &std::path::Path) -> Self {
            let prior = std::env::var("SWFC_CONFIG_DIR").ok();
            // SAFETY: mutation serialized by ENV_LOCK.
            unsafe {
                std::env::set_var("SWFC_CONFIG_DIR", value);
            }
            Self(prior)
        }
    }
    impl Drop for EnvGuard {
        fn drop(&mut self) {
            // SAFETY: mutation serialized by ENV_LOCK.
            match &self.0 {
                Some(v) => unsafe { std::env::set_var("SWFC_CONFIG_DIR", v) },
                None => unsafe { std::env::remove_var("SWFC_CONFIG_DIR") },
            }
        }
    }

    fn make_router() -> Router {
        Router::new().route("/api/chat/stage-descriptions", get(get_stage_descriptions))
    }

    async fn body_json(body: Body) -> serde_json::Value {
        let bytes = to_bytes(body, 1_000_000).await.unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[tokio::test]
    async fn returns_parsed_descriptions_from_config_dir() {
        let _lock = ENV_LOCK.lock().await;
        let tmp = tempfile::TempDir::new().unwrap();
        let yaml = r#"
version: 1
stages:
  data_acquisition:
    sme_friendly_name: "Data acquisition"
    short: "Pulls raw data."
    long: "Long form text."
    example_outputs: "a manifest"
  quality_control:
    sme_friendly_name: "Quality filtering"
    short: "Drops bad cells."
"#;
        std::fs::write(tmp.path().join("stage-descriptions.yaml"), yaml).unwrap();
        let _guard = EnvGuard::capture_and_set(tmp.path());
        let resp = make_router()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/api/chat/stage-descriptions")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp.into_body()).await;
        assert_eq!(body["version"], 1);
        assert_eq!(
            body["stages"]["data_acquisition"]["sme_friendly_name"],
            "Data acquisition"
        );
        assert_eq!(
            body["stages"]["data_acquisition"]["example_outputs"],
            "a manifest"
        );
        assert_eq!(
            body["stages"]["quality_control"]["short"],
            "Drops bad cells."
        );
        // optional fields omitted when absent.
        assert!(
            body["stages"]["quality_control"]
                .get("example_outputs")
                .is_none()
                || body["stages"]["quality_control"]["example_outputs"].is_null()
        );
    }

    #[tokio::test]
    async fn returns_empty_when_config_file_missing() {
        let _lock = ENV_LOCK.lock().await;
        let tmp = tempfile::TempDir::new().unwrap();
        let _guard = EnvGuard::capture_and_set(tmp.path());
        let resp = make_router()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/api/chat/stage-descriptions")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp.into_body()).await;
        assert_eq!(body["version"], 0);
        assert!(body["stages"].as_object().unwrap().is_empty());
    }
}
