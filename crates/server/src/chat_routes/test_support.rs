//! Shared test helpers for chat_routes tests. Lives outside `tests.rs`
//! so per-submodule `mod tests` blocks can pull them in without
//! duplication.
#![allow(unreachable_pub)]

use super::{ChatAppState, Router};
use axum::body::{to_bytes, Body};
use scripps_workflow_conversation::{
    anthropic::{StopReason, TurnResponse, Usage},
    LlmBackend, MockLlmBackend, SessionStore, Tool,
};
use std::path::PathBuf;
use std::sync::Arc;

/// Process-wide lock for tests that mutate `SWFC_SHARED_URLS_ENABLED`.
/// Lives here (not in each submodule's `mod tests`) so the
/// `chat_routes::share` and crate-root `read_only` test modules
/// serialize against a single mutex — otherwise cargo's parallel
/// runner can race and observe a half-set env var.
///
/// tokio::sync::Mutex (not std::sync::Mutex) so the guard is async-aware
/// and can be held across `.await` calls without tripping the
/// workspace-wide `clippy::await_holding_lock = "deny"` policy.
pub static SHARED_URLS_ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

pub fn config_dir() -> PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("config")
}

pub fn assistant(text: &str) -> TurnResponse {
    TurnResponse {
        assistant_content: text.into(),
        tool_uses: vec![],
        stop_reason: StopReason::EndTurn,
        usage: Usage::default(),
        request_metadata: Default::default(),
    }
}

pub fn tool_use(t: Tool) -> TurnResponse {
    TurnResponse {
        assistant_content: String::new(),
        tool_uses: vec![(uuid::Uuid::new_v4(), t)],
        stop_reason: StopReason::ToolUse,
        usage: Usage::default(),
        request_metadata: Default::default(),
    }
}

pub async fn make_router(scripted: Vec<TurnResponse>) -> (Router, ChatAppState) {
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::open(dir.path()).await.unwrap();
    std::mem::forget(dir);
    let backend: Arc<dyn LlmBackend> = Arc::new(MockLlmBackend::new(scripted));
    let app = ChatAppState::with_backend(backend, store, config_dir());
    // Layer a default `RequestPrincipal` extension so handlers that
    // extract `Extension<RequestPrincipal>` (C1 hardening) resolve
    // cleanly under the test router. Production installs this via
    // `auth::extract_principal` middleware; tests skip that middleware
    // and inject a bearer-authenticated `Owner { user: "local" }`
    // principal directly so handler logic — not the extractor — drives
    // the response code.
    let router = super::router(app.clone()).layer(axum::Extension(
        crate::auth::RequestPrincipal::test_default(),
    ));
    (router, app)
}

/// Like [`make_router`] but pins `app.auto_title_override = Some(true)`
/// so the auto-title handler reaches its real logic instead of
/// short-circuiting on the `SWFC_AUTO_TITLE` env-var gate. Lets the 5
/// auto-title tests avoid mutating the process-wide env table.
pub async fn make_router_with_auto_title_enabled(
    scripted: Vec<TurnResponse>,
) -> (Router, ChatAppState) {
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::open(dir.path()).await.unwrap();
    std::mem::forget(dir);
    let backend: Arc<dyn LlmBackend> = Arc::new(MockLlmBackend::new(scripted));
    let mut app = ChatAppState::with_backend(backend, store, config_dir());
    app.auto_title_override = Some(true);
    let router = super::router(app.clone()).layer(axum::Extension(
        crate::auth::RequestPrincipal::test_default(),
    ));
    (router, app)
}

pub async fn body_json(body: Body) -> serde_json::Value {
    let bytes = to_bytes(body, 1_000_000).await.unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

/// Seed a session with a single completed task in its DAG. Optional
/// `package_root` populates `emitted_package_path` so the
/// `get_task_result` artifact-listing path has a directory to scan.
pub async fn seed_session_with_completed_task(
    app: &ChatAppState,
    task_id: &str,
    package_root: Option<std::path::PathBuf>,
) -> uuid::Uuid {
    use scripps_workflow_core::dag::{
        Assignee, ResourceClass, Task, TaskId, TaskKind, TaskState, DAG,
    };
    let (id, _) = app.conversation.start_session(false).await.unwrap();
    let store = app.conversation.store_handle();
    store
        .update(id, |s| {
            let mut tasks = std::collections::BTreeMap::new();
            tasks.insert(
                TaskId::from(task_id),
                Task {
                    kind: TaskKind::Computation,
                    state: TaskState::Completed {
                        result: serde_json::json!({"metric": 42}),
                    },
                    depends_on: vec![],
                    assignee: Assignee::Agent,
                    description: "demo completed task".into(),
                    spec: None,
                    resolution: None,
                    result_ref: None,
                    resource_class: ResourceClass::CpuHeavy,
                    requires_sme_review: false,

                    required_artifacts: vec![],
                    container: None,
                    source_atom_id: None,
                    safety: Default::default(),
                },
            );
            s.dag = Some(DAG {
                version: "test".into(),
                schema_version: scripps_workflow_core::dag::current_dag_schema_version(),
                workflow_id: "workflow-test".into(),
                current_task: None,
                tasks,
                reverse_deps: std::collections::BTreeMap::new(),
                run_id: None,
            });
            s.emitted_package_path = package_root.clone();
            Ok(())
        })
        .await
        .unwrap();
    id
}
