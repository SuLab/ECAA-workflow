//! Shared test helpers for chat_routes tests. Lives outside `tests.rs`
//! so per-submodule `mod tests` blocks can pull them in without
//! duplication.
#![allow(unreachable_pub)]

use super::{ChatAppState, Router};
use axum::body::{to_bytes, Body};
use ecaa_workflow_conversation::{
    anthropic::{StopReason, TurnResponse, Usage},
    LlmBackend, MockLlmBackend, SessionStore, Tool,
};
use std::path::PathBuf;
use std::sync::Arc;

/// Process-wide lock for tests that mutate `ECAA_SHARED_URLS_ENABLED`.
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
    make_router_with_config(config_dir(), scripted).await
}

/// Like [`make_router`] but with a caller-supplied `config_dir` — used by the
/// SME-parameter + branch-to-edit tests that point the app at an
/// [`augmented_config`] tree carrying an atom with a `parameters:` block.
pub async fn make_router_with_config(
    config_dir: PathBuf,
    scripted: Vec<TurnResponse>,
) -> (Router, ChatAppState) {
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::open(dir.path()).await.unwrap();
    std::mem::forget(dir);
    let backend: Arc<dyn LlmBackend> = Arc::new(MockLlmBackend::new(scripted));
    let app = ChatAppState::with_backend(backend, store, config_dir);
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

/// Build a config dir that mirrors the repo `config/` (children symlinked) but
/// augments one stage-atom with a `parameters:` block, so the SME-parameter
/// endpoints + branch-to-edit have a real atom carrying a typed parameter
/// schema to serve/validate against. The temp dir is leaked so the symlinked
/// tree outlives the app under test.
pub fn augmented_config(atom_id: &str, param_yaml: &str) -> PathBuf {
    let repo = config_dir();
    let tmp = tempfile::tempdir().unwrap();
    let cfg = tmp.path().join("config");
    std::fs::create_dir_all(&cfg).unwrap();
    for entry in std::fs::read_dir(&repo).unwrap() {
        let entry = entry.unwrap();
        let name = entry.file_name();
        if name == "stage-atoms" {
            continue;
        }
        std::os::unix::fs::symlink(entry.path(), cfg.join(&name)).unwrap();
    }
    let atoms_src = repo.join("stage-atoms");
    let atoms_dst = cfg.join("stage-atoms");
    std::fs::create_dir_all(&atoms_dst).unwrap();
    let target_file = format!("{atom_id}.yaml");
    for entry in std::fs::read_dir(&atoms_src).unwrap() {
        let entry = entry.unwrap();
        let name = entry.file_name();
        if name.to_string_lossy() == target_file {
            continue;
        }
        std::os::unix::fs::symlink(entry.path(), atoms_dst.join(&name)).unwrap();
    }
    let original = std::fs::read_to_string(atoms_src.join(&target_file)).unwrap();
    // Merge the injected block into the atom, REPLACING any key of the same name
    // rather than appending. Real atoms now declare their own `parameters:`
    // block (config/stage-atoms/*.yaml), and a naive text append would produce a
    // duplicate `parameters:` key that breaks atom load (→ no composed DAG).
    let mut doc: serde_yaml_ng::Value = serde_yaml_ng::from_str(&original).unwrap();
    let inject: serde_yaml_ng::Value = serde_yaml_ng::from_str(param_yaml).unwrap();
    if let (Some(map), Some(inj)) = (doc.as_mapping_mut(), inject.as_mapping()) {
        for (k, v) in inj {
            map.insert(k.clone(), v.clone());
        }
    }
    let merged = serde_yaml_ng::to_string(&doc).unwrap();
    std::fs::write(atoms_dst.join(&target_file), merged).unwrap();
    std::mem::forget(tmp);
    cfg
}

/// Like [`make_router`] but pins `app.auto_title_override = Some(true)`
/// so the auto-title handler reaches its real logic instead of
/// short-circuiting on the `ECAA_AUTO_TITLE` env-var gate. Lets the 5
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

/// Like [`make_router`] but injects `harness_bin` into `app.config`'s
/// `harness_bin_path`, so execution tests can pin the spawned harness to
/// a stub binary (e.g. `/usr/bin/true`) WITHOUT mutating the
/// process-global `ECAA_HARNESS_BIN_PATH` env var. The env table is
/// shared across the multi-threaded test binary; mutating it raced the
/// harness-binary resolution in `spawn_harness_for_session_reserved`.
/// Mirrors `make_router_with_auto_title_enabled`'s `auto_title_override`.
pub async fn make_router_with_harness_bin(
    scripted: Vec<TurnResponse>,
    harness_bin: &str,
) -> (Router, ChatAppState) {
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::open(dir.path()).await.unwrap();
    std::mem::forget(dir);
    let backend: Arc<dyn LlmBackend> = Arc::new(MockLlmBackend::new(scripted));
    let mut app = ChatAppState::with_backend(backend, store, config_dir());
    let mut cfg = (*app.config).clone();
    cfg.harness_bin_path = Some(std::path::PathBuf::from(harness_bin));
    app.config = Arc::new(cfg);
    let router = super::router(app.clone()).layer(axum::Extension(
        crate::auth::RequestPrincipal::test_default(),
    ));
    (router, app)
}

/// Insert a running `ExecutionHandle` (exit_status = UNSET) into
/// `app.executions` for `session_id`, so tests that assert
/// replay↔execution mutual exclusion can simulate an in-flight harness
/// without actually spawning one. Mirrors the production
/// `ExecutionHandle::for_running` construction with a stub pid/token.
pub fn insert_running_execution(
    app: &ChatAppState,
    session_id: uuid::Uuid,
    package_dir: std::path::PathBuf,
) {
    let handle =
        super::ExecutionHandle::for_running(1, 1, package_dir, "test-agent".into(), [0u8; 32]);
    app.executions.insert(session_id, handle);
}

pub async fn body_json(body: Body) -> serde_json::Value {
    let bytes = to_bytes(body, 1_000_000).await.unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

/// Seed a session with a single `Running` task in its DAG that declares
/// `required_artifacts` (each with no min-size, "non-empty file" check),
/// and set `emitted_package_path` to `package_root`. Used by the CV-4
/// server-side artifact-guard tests: they then POST a `Completed`
/// transition and assert the guard refuses/accepts based on whether the
/// declared artifacts exist on disk.
pub async fn seed_session_with_task_requiring_artifacts(
    app: &ChatAppState,
    task_id: &str,
    package_root: std::path::PathBuf,
    artifact_paths: &[&str],
) -> uuid::Uuid {
    use ecaa_workflow_core::dag::{
        Assignee, RequiredArtifact, ResourceClass, Task, TaskId, TaskKind, TaskState, DAG,
    };
    let required: Vec<RequiredArtifact> = artifact_paths
        .iter()
        .map(|p| RequiredArtifact {
            path: (*p).to_string(),
            min_size_bytes: None,
            schema_ref: None,
            validation_obligations: Vec::new(),
        })
        .collect();
    let (id, _) = app.conversation.start_session(false).await.unwrap();
    let store = app.conversation.store_handle();
    let tid = task_id.to_string();
    store
        .update(id, move |s| {
            let mut tasks = std::collections::BTreeMap::new();
            tasks.insert(
                TaskId::from(tid.as_str()),
                Task {
                    kind: TaskKind::Computation,
                    state: TaskState::Running {
                        started_at: "2026-07-20T00:00:00Z".into(),
                        remote: None,
                    },
                    depends_on: vec![],
                    assignee: Assignee::Agent,
                    description: "task declaring required artifacts".into(),
                    spec: None,
                    resolution: None,
                    result_ref: None,
                    resource_class: ResourceClass::CpuHeavy,
                    requires_sme_review: false,
                    required_artifacts: required.clone(),
                    container: None,
                    source_atom_id: None,
                    safety: Default::default(),
                    inputs: Vec::new(),
                    outputs: Vec::new(),
                    edam_operation: None,
                    execution_index: None,
                },
            );
            s.dag = Some(DAG {
                version: "test".into(),
                schema_version: ecaa_workflow_core::dag::current_dag_schema_version(),
                workflow_id: "workflow-test".into(),
                current_task: None,
                tasks,
                reverse_deps: std::collections::BTreeMap::new(),
                run_id: None,
                execution_order: Vec::new(),
            });
            s.emitted_package_path = Some(package_root.clone());
            Ok(())
        })
        .await
        .unwrap();
    id
}

/// Seed a session with a single completed task in its DAG. Optional
/// `package_root` populates `emitted_package_path` so the
/// `get_task_result` artifact-listing path has a directory to scan.
pub async fn seed_session_with_completed_task(
    app: &ChatAppState,
    task_id: &str,
    package_root: Option<std::path::PathBuf>,
) -> uuid::Uuid {
    use ecaa_workflow_core::dag::{
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
                    inputs: Vec::new(),
                    outputs: Vec::new(),
                    edam_operation: None,
                    execution_index: None,
                },
            );
            s.dag = Some(DAG {
                version: "test".into(),
                schema_version: ecaa_workflow_core::dag::current_dag_schema_version(),
                workflow_id: "workflow-test".into(),
                current_task: None,
                tasks,
                reverse_deps: std::collections::BTreeMap::new(),
                run_id: None,
                execution_order: Vec::new(),
            });
            s.emitted_package_path = package_root.clone();
            Ok(())
        })
        .await
        .unwrap();
    id
}
