//! `--plan-only` dry-run for the harness. Loads a package's
//! `WORKFLOW.json`, validates the DAG, walks task ids in deterministic
//! order, and prints a per-task plan summary. Side-effect free: no
//! multiprocess lock, no executor provisioning, no agent invocation.

use crate::executor::{
    self, enforce_safety_policy, host_probe::resolve_high_water_for, sizing::resolve_instance_type,
    ExecutorArgs,
};
use crate::swfc_io::read_capped_default;
use anyhow::Context;
use ecaa_workflow_core::blocker::BlockerKind;
use ecaa_workflow_core::dag::{validate_dag_typed, Task, TaskState, DAG};
use std::path::Path;

/// Process exit codes. Returned by `run` and exit-mapped by the caller.
const EXIT_OK: i32 = 0;
const EXIT_VALIDATION_FAILED: i32 = 2;
const EXIT_SAFETY_BLOCKED: i32 = 3;

/// Entry point. Prints the plan to stdout and returns the desired
/// process exit code. Caller is responsible for `process::exit`.
pub fn run(package: &Path, executor_mode: &str) -> anyhow::Result<i32> {
    let mode = normalize_mode(executor_mode);
    let abs_pkg = package
        .canonicalize()
        .unwrap_or_else(|_| package.to_path_buf());

    let dag = load_dag(package).with_context(|| {
        format!(
            "loading WORKFLOW.json from {}",
            package.join("WORKFLOW.json").display()
        )
    })?;

    let exec_args = ExecutorArgs {
        package: package.display().to_string(),
        agent: String::new(),
        task_timeout_secs: 0,
    };
    let executor = executor::build(&mode, &exec_args)?;
    let executor_name = executor.name();
    let caps = executor.capabilities();
    drop(executor);

    let validation = validate_dag_typed(&dag);
    let validate_msg = match &validation {
        Ok(()) => "ok".to_string(),
        Err(e) => format!("{}", e),
    };

    println!("package: {}", abs_pkg.display());
    println!("executor: {}", executor_name);
    println!("tasks: {}", dag.tasks.len());
    println!("validate_dag: {}", validate_msg);
    println!();

    let task_ids: Vec<String> = dag.tasks.keys().map(|id| id.to_string()).collect();

    let id_w = task_ids.iter().map(|s| s.len()).max().unwrap_or(7).max(7);
    let stage_w = dag
        .tasks
        .values()
        .map(|t| stage_class_of(t).len())
        .max()
        .unwrap_or(11)
        .max(11);
    let atom_w = dag
        .tasks
        .values()
        .map(|t| atom_id_of(t).len())
        .max()
        .unwrap_or(14)
        .max(14);
    let state_w = dag
        .tasks
        .values()
        .map(|t| state_name(&t.state).len())
        .max()
        .unwrap_or(5)
        .max(9);

    println!(
        "{:id$}  {:stage$}  {:atom$}  {:state$}  {:>4}  {:<24}  instance_type",
        "task_id",
        "stage_class",
        "source_atom_id",
        "state",
        "deps",
        "would_dispatch",
        id = id_w,
        stage = stage_w,
        atom = atom_w,
        state = state_w,
    );

    let mut ok_count: usize = 0;
    let mut blocked_count: usize = 0;

    for task_id in &task_ids {
        let task = dag
            .tasks
            .get(task_id.as_str())
            .expect("task_id from dag.tasks.keys()");
        let stage_class = stage_class_of(task);
        let atom_id = atom_id_of(task);
        let state = state_name(&task.state);
        let dep_count = task.depends_on.len();
        let would = enforce_safety_policy(task, &caps);
        let would_str = match &would {
            None => {
                ok_count += 1;
                "OK".to_string()
            }
            Some(b) => {
                blocked_count += 1;
                blocker_variant_name(b).to_string()
            }
        };
        let req = resolve_high_water_for(package, &dag, task_id);
        let instance = resolve_instance_type(&req);

        println!(
            "{:id$}  {:stage$}  {:atom$}  {:state$}  {:>4}  {:<24}  {}",
            task_id,
            stage_class,
            atom_id,
            state,
            dep_count,
            would_str,
            instance,
            id = id_w,
            stage = stage_w,
            atom = atom_w,
            state = state_w,
        );
    }

    println!();
    println!(
        "summary: {} dispatchable, {} blocked by safety policy",
        ok_count, blocked_count,
    );

    if validation.is_err() {
        return Ok(EXIT_VALIDATION_FAILED);
    }
    if blocked_count > 0 {
        return Ok(EXIT_SAFETY_BLOCKED);
    }
    Ok(EXIT_OK)
}

fn normalize_mode(requested: &str) -> String {
    // The `dry-run` cargo feature registers `mock` as a production-buildable
    // executor; without it `executor::build` rejects the mode, so plan-only
    // falls back to `local` (with a stderr notice) to stay useful.
    #[cfg(feature = "dry-run")]
    if requested == "mock" {
        return "mock".to_string();
    }
    #[cfg(not(feature = "dry-run"))]
    if requested == "mock" {
        eprintln!(
            "[plan-only] ECAA_EXECUTOR_MODE=mock is not supported by executor::build; falling back to 'local'"
        );
        return "local".to_string();
    }
    requested.to_string()
}

fn load_dag(package: &Path) -> anyhow::Result<DAG> {
    let content =
        read_capped_default(&package.join("WORKFLOW.json")).context("reading WORKFLOW.json")?;
    let dag: DAG = serde_json::from_str(&content).context("parsing WORKFLOW.json")?;
    Ok(dag)
}

fn stage_class_of(task: &Task) -> String {
    task.spec
        .as_ref()
        .and_then(|s| s.get("stage_class"))
        .and_then(|v| v.as_str())
        .unwrap_or("-")
        .to_string()
}

fn atom_id_of(task: &Task) -> String {
    task.source_atom_id.as_deref().unwrap_or("-").to_string()
}

fn state_name(state: &TaskState) -> &'static str {
    match state {
        TaskState::Pending => "pending",
        TaskState::Ready => "ready",
        TaskState::Running { .. } => "running",
        TaskState::Completed { .. } => "completed",
        TaskState::Failed { .. } => "failed",
        TaskState::Blocked { .. } => "blocked",
    }
}

fn blocker_variant_name(b: &BlockerKind) -> &'static str {
    match b {
        BlockerKind::SandboxRequired { .. } => "SandboxRequired",
        BlockerKind::NetworkPolicyMismatch { .. } => "NetworkPolicyMismatch",
        // `BlockerKind` is `#[non_exhaustive]`; fall back honestly rather
        // than fabricate a synthetic discriminant name.
        _ => "<other>",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ecaa_workflow_core::atom::{
        NetworkPolicy, SafetyLevel, SafetyPolicy, SandboxRequirement,
    };
    use ecaa_workflow_core::dag::{Assignee, Task, TaskKind, TaskState, DAG};
    use ecaa_workflow_core::ids::TaskId;
    use std::collections::BTreeMap;
    use std::fs::File;
    use std::io::Write;

    fn empty_task() -> Task {
        Task {
            kind: TaskKind::Computation,
            state: TaskState::Pending,
            depends_on: vec![],
            assignee: Assignee::Agent,
            description: String::from("test"),
            spec: None,
            resolution: None,
            result_ref: None,
            resource_class: Default::default(),
            requires_sme_review: false,
            required_artifacts: vec![],
            container: None,
            source_atom_id: None,
            safety: SafetyPolicy::default(),
        }
    }

    fn make_dag(tasks: BTreeMap<TaskId, Task>) -> DAG {
        DAG {
            version: "1.0".to_string(),
            schema_version: ecaa_workflow_core::dag::current_dag_schema_version(),
            workflow_id: "test-plan-only".to_string(),
            current_task: None,
            tasks,
            run_id: None,
            reverse_deps: BTreeMap::new(),
        }
    }

    fn write_package(dir: &Path, dag: &DAG) {
        std::fs::create_dir_all(dir).unwrap();
        let path = dir.join("WORKFLOW.json");
        let mut f = File::create(&path).unwrap();
        let body = serde_json::to_vec_pretty(dag).unwrap();
        f.write_all(&body).unwrap();
    }

    #[test]
    fn clean_dag_returns_zero() {
        use ecaa_workflow_core::ids::TaskId;
        let dir = tempfile::tempdir().unwrap();
        let mut tasks = BTreeMap::new();
        let task_a = empty_task();
        let mut task_b = empty_task();
        task_b.depends_on = vec![TaskId::from("a")];
        tasks.insert(TaskId::from("a"), task_a);
        tasks.insert(TaskId::from("b"), task_b);
        let dag = make_dag(tasks);
        write_package(dir.path(), &dag);

        let code = run(dir.path(), "local").expect("run must not error");
        assert_eq!(code, 0, "clean DAG must return EXIT_OK");
    }

    #[test]
    fn cyclic_dag_returns_two() {
        use ecaa_workflow_core::ids::TaskId;
        let dir = tempfile::tempdir().unwrap();
        let mut tasks = BTreeMap::new();
        let mut a = empty_task();
        a.depends_on = vec![TaskId::from("b")];
        let mut b = empty_task();
        b.depends_on = vec![TaskId::from("a")];
        tasks.insert(TaskId::from("a"), a);
        tasks.insert(TaskId::from("b"), b);
        let dag = make_dag(tasks);
        write_package(dir.path(), &dag);

        let code = run(dir.path(), "local").expect("run must not error");
        assert_eq!(code, 2, "cyclic DAG must return EXIT_VALIDATION_FAILED");
    }

    #[test]
    fn dangling_dep_returns_two() {
        use ecaa_workflow_core::ids::TaskId;
        let dir = tempfile::tempdir().unwrap();
        let mut tasks = BTreeMap::new();
        let mut a = empty_task();
        a.depends_on = vec![TaskId::from("ghost")];
        tasks.insert(TaskId::from("a"), a);
        let dag = make_dag(tasks);
        write_package(dir.path(), &dag);

        let code = run(dir.path(), "local").expect("run must not error");
        assert_eq!(code, 2, "dangling dep must return EXIT_VALIDATION_FAILED");
    }

    #[test]
    fn safety_mismatch_returns_three() {
        use ecaa_workflow_core::ids::TaskId;
        // Force LocalExecutor's sandbox capability to None so an Exec
        // atom requesting HardwareEnclave is guaranteed unsatisfiable
        // regardless of the host's bwrap presence.
        let dir = tempfile::tempdir().unwrap();
        let mut tasks = BTreeMap::new();
        let mut a = empty_task();
        a.safety = SafetyPolicy {
            level: SafetyLevel::Exec,
            network: NetworkPolicy::Bridge,
            code_execution: Default::default(),
            sandbox: SandboxRequirement::HardwareEnclave,
            provisioning: Default::default(),
            controlled_access: false,
        };
        let mut b = empty_task();
        b.depends_on = vec![TaskId::from("a")];
        tasks.insert(TaskId::from("a"), a);
        tasks.insert(TaskId::from("b"), b);
        let dag = make_dag(tasks);
        write_package(dir.path(), &dag);

        let prev = std::env::var("ECAA_LOCAL_SANDBOX").ok();
        std::env::set_var("ECAA_LOCAL_SANDBOX", "off");
        let code = run(dir.path(), "local").expect("run must not error");
        match prev {
            Some(v) => std::env::set_var("ECAA_LOCAL_SANDBOX", v),
            None => std::env::remove_var("ECAA_LOCAL_SANDBOX"),
        }
        assert_eq!(
            code, 3,
            "safety mismatch (HardwareEnclave required, none available) must return EXIT_SAFETY_BLOCKED"
        );
    }
}
