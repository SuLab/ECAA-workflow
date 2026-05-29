//! Integration tests for the `executor::build` factory and trait
//! dispatch. Live here (not in `src/executor/mod.rs`) so they exercise
//! the public library surface the way downstream callers will.
//!
//! A PATH-shimmed `aws` scaffold below lets AwsExecutor be driven
//! end-to-end without touching real cloud. The shim records every
//! `aws` invocation as a JSONL line and dispatches canned responses by
//! matching a (subcommand, operation) pair. Tests hold the shared PATH
//! mutex so environment mutation can't race across threads.

// S5.32: workspace lint is `unsafe_code = "deny"`. This integration
// file uses `unsafe { std::env::set_var / remove_var }` to control
// ECAA_* envs (unsafe in Rust 2024 edition because the env table is
// not thread-safe). All call sites take the shared PATH mutex above
// so mutation can't race; the bounded waiver is scoped to this
// integration test target.
#![allow(unsafe_code)]

use ecaa_workflow_core::dag::{
    Assignee, BlockedRecord, RemoteExecution, ResourceClass, Task, TaskId, TaskKind, TaskState, DAG,
};
use ecaa_workflow_harness::executor::{aws::AwsExecutor, build, Executor, ExecutorArgs};
use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

fn args() -> ExecutorArgs {
    ExecutorArgs {
        package: "/tmp/harness-executor-integration".into(),
        agent: "/bin/true".into(),
        task_timeout_secs: 300,
    }
}

/// Shared mutex so tests mutating PATH / ECAA_AWS_* env vars run
/// one-at-a-time. `cargo test` schedules tests across multiple threads
/// by default; each shim test grabs this lock for its duration.
static ENV_LOCK: Mutex<()> = Mutex::new(());

#[test]
fn factory_returns_error_for_unknown_mode() {
    //.err() rather than.expect_err() because the Ok variant
    // (`Box<dyn Executor>`) does not implement Debug.
    let err = build("kubernetes", &args())
        .err()
        .expect("kubernetes is not a known mode");
    let msg = err.to_string();
    assert!(
        msg.contains("Unknown ECAA_EXECUTOR_MODE"),
        "error should mention the env var, got: {msg}"
    );
    assert!(
        msg.contains("kubernetes"),
        "error should echo the bad mode, got: {msg}"
    );
    assert!(
        msg.contains("local") && msg.contains("aws"),
        "error should list valid modes, got: {msg}"
    );
}

#[test]
fn factory_returns_local_executor_for_local() {
    let exec = build("local", &args()).expect("local mode must construct");
    assert_eq!(exec.name(), "local");
}

#[test]
fn factory_returns_error_for_aws_without_required_env_vars() {
    // With no ECAA_AWS_* env vars set, the factory must reject with
    // a single diagnostic listing every missing variable.
    for k in [
        "ECAA_AWS_REGION",
        "ECAA_AWS_AMI_ID",
        "ECAA_AWS_SECURITY_GROUP",
        "ECAA_AWS_INSTANCE_PROFILE",
        "ECAA_AWS_SUBNET_ID",
        "ECAA_AWS_SUBNET_IDS",
    ] {
        unsafe { std::env::remove_var(k) };
    }
    let err = build("aws", &args())
        .err()
        .expect("aws requires env config");
    let msg = err.to_string();
    assert!(
        msg.contains("missing required env vars"),
        "error must list missing env vars, got: {msg}"
    );
    assert!(
        msg.contains("ECAA_AWS_REGION"),
        "error must enumerate each missing var, got: {msg}"
    );
}

#[test]
fn local_executor_is_task_stale_matches_timestamp_threshold() {
    let exec = build("local", &args()).expect("local");

    let mut old_task = Task {
        kind: TaskKind::Computation,
        state: TaskState::Running {
            started_at: "2020-01-01T00:00:00Z".into(), // far in the past
            remote: None,
        },
        depends_on: vec![],
        assignee: Assignee::Agent,
        description: "old running task".into(),
        spec: None,
        resolution: None,
        result_ref: None,
        resource_class: ResourceClass::CpuHeavy,
        requires_sme_review: false,

        required_artifacts: vec![],
        container: None,
        source_atom_id: None,
        safety: Default::default(),
    };
    let now = chrono::Utc::now().timestamp() as u64;
    assert!(
        exec.is_task_stale(&old_task, now),
        "a 6-year-old Running task must be stale under the 300s threshold"
    );

    // Replace with a fresh timestamp — should NOT be stale.
    old_task.state = TaskState::Running {
        started_at: chrono::Utc::now().to_rfc3339(),
        remote: None,
    };
    assert!(
        !exec.is_task_stale(&old_task, now),
        "a just-started task must not be stale"
    );

    // Non-Running states must never be stale regardless of timestamp.
    old_task.state = TaskState::Ready;
    assert!(
        !exec.is_task_stale(&old_task, now),
        "Ready tasks are never stale"
    );
    old_task.state = TaskState::Pending;
    assert!(
        !exec.is_task_stale(&old_task, now),
        "Ready tasks are never stale"
    );
}

// AWS dry-run scaffold. Each shim invocation is recorded as one JSONL
// line containing `{"args":[...], "cwd":<path>}`. The shim dispatches
// a canned response by grep-matching the argv list against rules
// stored in a `responses` directory — first matching file wins. Rule
// files are named `<priority>-<pattern>.json` where `<pattern>` is a
// space-joined substring that must appear in the argv; priority sorts
// lexically.

/// Install a PATH-shimmed `aws` binary. Returns the bin dir (to prepend
/// to PATH) and the log path (for per-invocation JSONL assertions).
///
/// The shim is a Python script because the tab-separated-index approach
/// in pure POSIX sh fought with various `read` semantics across
/// distributions. Python is available on every CI runner and gives us
/// a clean glob-and-cat loop.
fn install_shim(scratch: &Path, responses: &BTreeMap<&str, &str>) -> (PathBuf, PathBuf) {
    let bin_dir = scratch.join("bin");
    std::fs::create_dir_all(&bin_dir).unwrap();
    let log = scratch.join("aws-invocations.jsonl");
    let rules_dir = scratch.join("responses");
    std::fs::create_dir_all(&rules_dir).unwrap();

    // One file per rule: pattern JSON + body JSON. Ordering is by
    // BTreeMap key — matching is first-wins so more specific rules
    // should come first alphabetically.
    let mut rules: Vec<(String, String)> = Vec::new();
    for (i, (pattern, body)) in responses.iter().enumerate() {
        let filename = format!("rule-{:03}.json", i);
        let body_path = rules_dir.join(&filename);
        std::fs::write(&body_path, body).unwrap();
        rules.push((pattern.to_string(), body_path.display().to_string()));
    }
    let rules_json = serde_json::to_string_pretty(&rules).unwrap();
    let rules_index = rules_dir.join("rules.json");
    std::fs::write(&rules_index, rules_json).unwrap();

    let aws_path = bin_dir.join("aws");
    let script = format!(
        r#"#!/usr/bin/env python3
import json
import os
import sys

argv = sys.argv[1:]
log_path = {log:?}
rules_path = {rules:?}

with open(log_path, "a") as f:
    f.write(json.dumps({{"args": argv, "cwd": os.getcwd()}}) + "\n")

argv_str = " ".join(argv)
with open(rules_path) as f:
    rules = json.load(f)
for pattern, body_path in rules:
    if pattern in argv_str:
        with open(body_path) as bf:
            sys.stdout.write(bf.read())
        sys.exit(0)
sys.stdout.write("{{}}")
sys.exit(0)
"#,
        log = log.display().to_string(),
        rules = rules_index.display().to_string(),
    );
    std::fs::write(&aws_path, script).unwrap();
    let mut perms = std::fs::metadata(&aws_path).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&aws_path, perms).unwrap();
    (bin_dir, log)
}

fn read_invocations(log: &Path) -> Vec<serde_json::Value> {
    std::fs::read_to_string(log)
        .unwrap_or_default()
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

fn invocation_has(inv: &serde_json::Value, needle: &str) -> bool {
    let args = inv["args"].as_array().cloned().unwrap_or_default();
    args.iter()
        .any(|a| a.as_str().map(|s| s.contains(needle)).unwrap_or(false))
}

fn invocations_contain_subcommand(invs: &[serde_json::Value], words: &[&str]) -> bool {
    invs.iter().any(|inv| {
        let args = inv["args"].as_array().cloned().unwrap_or_default();
        let joined: Vec<String> = args
            .iter()
            .map(|a| a.as_str().unwrap_or("").to_string())
            .collect();
        joined
            .windows(words.len())
            .any(|w| w.iter().zip(words.iter()).all(|(g, e)| g == e))
    })
}

/// Set each env var to the given value for the lifetime of the returned
/// guard. Drop restores the prior values (or removes when absent).
struct EnvGuard {
    keys: Vec<String>,
    prior: Vec<Option<String>>,
}

impl EnvGuard {
    fn new(set: &[(&str, &str)]) -> Self {
        let mut prior = Vec::new();
        let mut keys = Vec::new();
        for (k, v) in set {
            prior.push(std::env::var(k).ok());
            unsafe { std::env::set_var(k, v) };
            keys.push((*k).to_string());
        }
        Self { keys, prior }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        for (k, v) in self.keys.iter().zip(self.prior.iter()) {
            match v {
                Some(val) => unsafe { std::env::set_var(k, val) },
                None => unsafe { std::env::remove_var(k) },
            }
        }
    }
}

/// Prepend the given dir to PATH for the lifetime of the guard.
struct PathGuard {
    prior: Option<String>,
}

impl PathGuard {
    fn prepend(dir: &Path) -> Self {
        let prior = std::env::var("PATH").ok();
        let new = match &prior {
            Some(p) => format!("{}:{}", dir.display(), p),
            None => dir.display().to_string(),
        };
        unsafe { std::env::set_var("PATH", &new) };
        Self { prior }
    }
}

impl Drop for PathGuard {
    fn drop(&mut self) {
        match &self.prior {
            Some(p) => unsafe { std::env::set_var("PATH", p) },
            None => unsafe { std::env::remove_var("PATH") },
        }
    }
}

/// Build an `AwsExecutor` against the current env (caller already set
/// the required ECAA_AWS_* vars via EnvGuard).
fn make_executor(package: &str, timeout_secs: u64) -> AwsExecutor {
    let exec_args = ExecutorArgs {
        package: package.into(),
        agent: "/opt/ecaa-workflow/run-task-on-instance.sh".into(),
        task_timeout_secs: timeout_secs,
    };
    AwsExecutor::new(&exec_args).expect("config")
}

fn required_env() -> Vec<(&'static str, &'static str)> {
    vec![
        ("ECAA_AWS_REGION", "us-west-2"),
        ("ECAA_AWS_AMI_ID", "ami-test"),
        ("ECAA_AWS_SECURITY_GROUP", "sg-test"),
        ("ECAA_AWS_INSTANCE_PROFILE", "scripps-agent"),
        ("ECAA_AWS_SUBNET_IDS", "subnet-only"),
        // `do_provision` runs the cost guard before `ec2 run-instances`;
        // an unset ceiling fails closed with `CostGuardError::CeilingUnset`.
        // Shim-backed tests don't model real spend, so set deliberately
        // high ceilings on BOTH the per-provision and run-total ceilings
        // so the guard always passes. The run-total ceiling was added in
        // commit 9322b924 (W5.1 fail-closed cumulative guard); test
        // fixtures were missed, surfacing as "CeilingUnset" panics that
        // misleadingly blamed only the per-provision env var.
        ("ECAA_AWS_COST_CEILING_USD", "1000000"),
        ("ECAA_AWS_RUN_TOTAL_CEILING_USD", "1000000"),
    ]
}

/// Write a minimal WORKFLOW.json with one ready task into `pkg`.
fn seed_ready_dag(pkg: &Path, task_id: &str) {
    let mut tasks = BTreeMap::new();
    tasks.insert(
        TaskId::from(task_id),
        Task {
            kind: TaskKind::Computation,
            state: TaskState::Ready,
            depends_on: vec![],
            assignee: Assignee::Agent,
            description: "test task".into(),
            spec: Some(serde_json::json!({
                "stage_class": "alignment_quantification",
                "task_id": task_id,
            })),
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
    let dag = DAG {
        version: "1.0".into(),
        schema_version: ecaa_workflow_core::dag::current_dag_schema_version(),
        workflow_id: "dry-run".into(),
        current_task: None,
        tasks,
        reverse_deps: std::collections::BTreeMap::new(),
        run_id: None,
    };
    std::fs::create_dir_all(pkg).unwrap();
    std::fs::write(
        pkg.join("WORKFLOW.json"),
        serde_json::to_string_pretty(&dag).unwrap(),
    )
    .unwrap();
}

fn read_dag(pkg: &Path) -> DAG {
    let raw = std::fs::read_to_string(pkg.join("WORKFLOW.json")).unwrap();
    serde_json::from_str(&raw).unwrap()
}

/// Canned responses for the full-lifecycle run_iteration test. Covers
/// run-instances (provision), describe-instance-information (wait_for_ssm),
/// send-command, and list-command-invocations.
fn canned_run_iteration_success(exit_code: i64) -> BTreeMap<&'static str, String> {
    let mut responses = BTreeMap::new();
    responses.insert(
        "run-instances",
        r#"{"Instances":[{"InstanceId":"i-shim-001"}]}"#.to_string(),
    );
    responses.insert(
        "describe-instance-information",
        r#"{"InstanceInformationList":[{"InstanceId":"i-shim-001","PingStatus":"Online"}]}"#
            .to_string(),
    );
    responses.insert(
        "send-command",
        r#"{"Command":{"CommandId":"cmd-shim-123"}}"#.to_string(),
    );
    responses.insert(
        "list-command-invocations",
        format!(
            r#"{{"CommandInvocations":[{{"CommandId":"cmd-shim-123","InstanceId":"i-shim-001","Status":"{status}","ResponseCode":{code},"StandardOutputContent":"ok","StandardErrorContent":""}}]}}"#,
            status = if exit_code == 0 { "Success" } else { "Failed" },
            code = exit_code,
        ),
    );
    responses
}

fn canned_describe_running_instance() -> &'static str {
    r#"{"Reservations":[{"Instances":[{"InstanceId":"i-shim-001","State":{"Name":"running"},"Events":[]}]}]}"#
}

fn canned_describe_terminated_instance() -> &'static str {
    r#"{"Reservations":[{"Instances":[{"InstanceId":"i-shim-001","State":{"Name":"terminated"},"Events":[]}]}]}"#
}

fn as_str_map<'a>(m: &'a BTreeMap<&'static str, String>) -> BTreeMap<&'a str, &'a str> {
    m.iter().map(|(k, v)| (*k, v.as_str())).collect()
}

/// Provision the executor via the shim so the internal `instance` field
/// is populated and run_iteration / is_task_stale can find an id.
fn provision_with_shim(exec: &mut AwsExecutor, pkg: &Path) {
    let dag = read_dag(pkg);
    exec.provision(&dag).expect("provision");
}

// AWS shim-backed tests

#[test]
fn run_iteration_sends_ssm_command_with_expected_parameters() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let scratch = tempfile::tempdir().unwrap();
    let pkg = scratch.path().join("pkg");
    seed_ready_dag(&pkg, "align-42");

    let responses = canned_run_iteration_success(0);
    let (bin_dir, log) = install_shim(scratch.path(), &as_str_map(&responses));

    let _env = EnvGuard::new(&required_env());
    let _path = PathGuard::prepend(&bin_dir);

    let mut exec = make_executor(pkg.to_str().unwrap(), 300);
    provision_with_shim(&mut exec, &pkg);
    let outcome = exec
        .run_iteration(
            &pkg,
            "/opt/ecaa-workflow/run-task-on-instance.sh",
            &std::collections::BTreeMap::new(),
        )
        .expect("run_iteration");
    assert!(outcome.agent_status.success());
    let remote = outcome.remote.expect("remote info attached");
    assert_eq!(remote.backend, "aws");
    assert_eq!(remote.instance_id, "i-shim-001");

    let invs = read_invocations(&log);
    let send_cmd = invs
        .iter()
        .find(|inv| invocation_has(inv, "send-command"))
        .expect("send-command recorded");
    // Document name, target instance and parameters are all present.
    assert!(invocation_has(send_cmd, "AWS-RunShellScript"));
    assert!(invocation_has(send_cmd, "i-shim-001"));
    let params = send_cmd["args"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a.as_str().unwrap_or("").starts_with(r#"{"commands""#))
        .expect("parameters arg present")
        .as_str()
        .unwrap()
        .to_string();
    assert!(
        params.contains("ECAA_TASK_ID=align-42"),
        "parameters must embed the task id, got: {}",
        params
    );
    assert!(
        params.contains("run-task-on-instance.sh"),
        "parameters must invoke the wrapper, got: {}",
        params
    );
    // Task was marked Completed.
    let after = read_dag(&pkg);
    assert!(matches!(
        after.tasks["align-42"].state,
        TaskState::Completed { .. }
    ));
}

#[test]
fn run_iteration_marks_task_failed_on_nonzero_exit() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let scratch = tempfile::tempdir().unwrap();
    let pkg = scratch.path().join("pkg");
    seed_ready_dag(&pkg, "fail-1");

    let responses = canned_run_iteration_success(2);
    let (bin_dir, _log) = install_shim(scratch.path(), &as_str_map(&responses));

    let _env = EnvGuard::new(&required_env());
    let _path = PathGuard::prepend(&bin_dir);

    let mut exec = make_executor(pkg.to_str().unwrap(), 300);
    provision_with_shim(&mut exec, &pkg);
    let outcome = exec
        .run_iteration(
            &pkg,
            "/opt/ecaa-workflow/run-task-on-instance.sh",
            &std::collections::BTreeMap::new(),
        )
        .expect("run_iteration ok wrapper, failed inner");
    assert!(
        !outcome.agent_status.success(),
        "non-zero agent exit expected"
    );

    let after = read_dag(&pkg);
    assert!(matches!(
        after.tasks["fail-1"].state,
        TaskState::Failed { .. }
    ));
}

#[test]
fn ensure_alive_reprovisions_on_terminated_instance() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let scratch = tempfile::tempdir().unwrap();
    let pkg = scratch.path().join("pkg");
    seed_ready_dag(&pkg, "t-1");

    // Shim serves:
    // - run-instances (for both provisions)
    // - describe-instance-information (wait_for_ssm)
    // - describe-instances with explicit --instance-ids (ensure_alive probe)
    let mut responses = canned_run_iteration_success(0);
    responses.insert(
        "describe-instances",
        canned_describe_terminated_instance().to_string(),
    );
    responses.insert("terminate-instances", r#"{}"#.to_string());
    let (bin_dir, log) = install_shim(scratch.path(), &as_str_map(&responses));

    let _env = EnvGuard::new(&required_env());
    let _path = PathGuard::prepend(&bin_dir);

    let mut exec = make_executor(pkg.to_str().unwrap(), 300);
    provision_with_shim(&mut exec, &pkg);
    // One provision so far.
    let invs_before = read_invocations(&log);
    let run_count_before = invs_before
        .iter()
        .filter(|i| invocation_has(i, "run-instances"))
        .count();
    assert_eq!(run_count_before, 1);

    let dag = read_dag(&pkg);
    exec.ensure_alive(&dag).expect("ensure_alive");

    let invs_after = read_invocations(&log);
    let run_count_after = invs_after
        .iter()
        .filter(|i| invocation_has(i, "run-instances"))
        .count();
    assert!(
        run_count_after >= 2,
        "terminated instance must trigger a reprovision; run-instances count was {}",
        run_count_after
    );
    // A terminate-instances was issued to clean up the stale handle.
    assert!(invocations_contain_subcommand(
        &invs_after,
        &["ec2", "terminate-instances"],
    ));
}

#[test]
fn ensure_alive_is_noop_on_running_instance() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let scratch = tempfile::tempdir().unwrap();
    let pkg = scratch.path().join("pkg");
    seed_ready_dag(&pkg, "t-1");

    let mut responses = canned_run_iteration_success(0);
    responses.insert(
        "describe-instances",
        canned_describe_running_instance().to_string(),
    );
    let (bin_dir, log) = install_shim(scratch.path(), &as_str_map(&responses));

    let _env = EnvGuard::new(&required_env());
    let _path = PathGuard::prepend(&bin_dir);

    let mut exec = make_executor(pkg.to_str().unwrap(), 300);
    provision_with_shim(&mut exec, &pkg);
    let invs_before = read_invocations(&log);
    let run_count_before = invs_before
        .iter()
        .filter(|i| invocation_has(i, "run-instances"))
        .count();

    let dag = read_dag(&pkg);
    exec.ensure_alive(&dag).expect("ensure_alive");

    let invs_after = read_invocations(&log);
    let run_count_after = invs_after
        .iter()
        .filter(|i| invocation_has(i, "run-instances"))
        .count();
    assert_eq!(
        run_count_before, run_count_after,
        "running instance must not trigger reprovision"
    );
    // No terminate-instances fired either.
    assert!(!invocations_contain_subcommand(
        &invs_after,
        &["ec2", "terminate-instances"],
    ));
}

#[test]
fn is_task_stale_false_for_fresh_running_task() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let scratch = tempfile::tempdir().unwrap();
    let pkg = scratch.path().join("pkg");
    seed_ready_dag(&pkg, "fresh-1");

    let responses = canned_run_iteration_success(0);
    let (bin_dir, _log) = install_shim(scratch.path(), &as_str_map(&responses));

    let _env = EnvGuard::new(&required_env());
    let _path = PathGuard::prepend(&bin_dir);

    let mut exec = make_executor(pkg.to_str().unwrap(), 3600);
    provision_with_shim(&mut exec, &pkg);

    let task = Task {
        kind: TaskKind::Computation,
        state: TaskState::Running {
            started_at: chrono::Utc::now().to_rfc3339(),
            remote: Some(RemoteExecution {
                backend: "aws".into(),
                instance_id: "i-shim-001".into(),
                instance_type: "r6i.2xlarge".into(),
                command_id: None,
                output_uri: None,
            }),
        },
        depends_on: vec![],
        assignee: Assignee::Agent,
        description: "fresh".into(),
        spec: Some(serde_json::json!({
            "stage_class": "alignment_quantification",
            "task_id": "fresh-1",
        })),
        resolution: None,
        result_ref: None,
        resource_class: ResourceClass::CpuHeavy,
        requires_sme_review: false,

        required_artifacts: vec![],
        container: None,
        source_atom_id: None,
        safety: Default::default(),
    };
    let now = chrono::Utc::now().timestamp() as u64;
    assert!(
        !exec.is_task_stale(&task, now),
        "a just-started task below the timeout must not be stale"
    );
}

#[test]
fn is_task_stale_true_when_ssm_reports_no_invocation() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let scratch = tempfile::tempdir().unwrap();
    let pkg = scratch.path().join("pkg");
    seed_ready_dag(&pkg, "past-1");

    let mut responses = canned_run_iteration_success(0);
    // SSM returns an empty invocations array = nothing ran / instance
    // probably restarted. is_task_stale must report TRUE.
    responses.insert(
        "list-command-invocations",
        r#"{"CommandInvocations":[]}"#.to_string(),
    );
    let (bin_dir, _log) = install_shim(scratch.path(), &as_str_map(&responses));

    let _env = EnvGuard::new(&required_env());
    let _path = PathGuard::prepend(&bin_dir);

    // Tiny timeout so the timestamp check passes straight to SSM.
    let mut exec = make_executor(pkg.to_str().unwrap(), 60);
    provision_with_shim(&mut exec, &pkg);

    let started = chrono::Utc::now() - chrono::Duration::seconds(7200);
    let task = Task {
        kind: TaskKind::Computation,
        state: TaskState::Running {
            started_at: started.to_rfc3339(),
            remote: Some(RemoteExecution {
                backend: "aws".into(),
                instance_id: "i-shim-001".into(),
                instance_type: "r6i.2xlarge".into(),
                command_id: None,
                output_uri: None,
            }),
        },
        depends_on: vec![],
        assignee: Assignee::Agent,
        description: "very old".into(),
        spec: Some(serde_json::json!({
            "stage_class": "alignment_quantification",
            "task_id": "past-1",
        })),
        resolution: None,
        result_ref: None,
        resource_class: ResourceClass::CpuHeavy,
        requires_sme_review: false,

        required_artifacts: vec![],
        container: None,
        source_atom_id: None,
        safety: Default::default(),
    };
    let now = chrono::Utc::now().timestamp() as u64;
    assert!(
        exec.is_task_stale(&task, now),
        "expired task with no SSM record must be stale"
    );
}

#[test]
fn is_task_stale_false_when_ssm_reports_success() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let scratch = tempfile::tempdir().unwrap();
    let pkg = scratch.path().join("pkg");
    seed_ready_dag(&pkg, "done-1");

    let mut responses = canned_run_iteration_success(0);
    responses.insert(
        "list-command-invocations",
        r#"{"CommandInvocations":[{"Status":"Success","ResponseCode":0}]}"#.to_string(),
    );
    let (bin_dir, _log) = install_shim(scratch.path(), &as_str_map(&responses));

    let _env = EnvGuard::new(&required_env());
    let _path = PathGuard::prepend(&bin_dir);

    let mut exec = make_executor(pkg.to_str().unwrap(), 60);
    provision_with_shim(&mut exec, &pkg);

    let started = chrono::Utc::now() - chrono::Duration::seconds(7200);
    let task = Task {
        kind: TaskKind::Computation,
        state: TaskState::Running {
            started_at: started.to_rfc3339(),
            remote: Some(RemoteExecution {
                backend: "aws".into(),
                instance_id: "i-shim-001".into(),
                instance_type: "r6i.2xlarge".into(),
                command_id: None,
                output_uri: None,
            }),
        },
        depends_on: vec![],
        assignee: Assignee::Agent,
        description: "finished but unobserved".into(),
        spec: Some(serde_json::json!({
            "stage_class": "alignment_quantification",
            "task_id": "done-1",
        })),
        resolution: None,
        result_ref: None,
        resource_class: ResourceClass::CpuHeavy,
        requires_sme_review: false,

        required_artifacts: vec![],
        container: None,
        source_atom_id: None,
        safety: Default::default(),
    };
    let now = chrono::Utc::now().timestamp() as u64;
    assert!(
        !exec.is_task_stale(&task, now),
        "task SSM reports Success for is not stale — caller should observe completion"
    );
}

#[test]
fn is_task_stale_honors_per_stage_ssm_timeout_override() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let scratch = tempfile::tempdir().unwrap();
    let pkg = scratch.path().join("pkg");
    seed_ready_dag(&pkg, "long-running-1");

    // Drop a per-stage profile into the package so
    // resolve_ssm_timeout_for_stage finds a 14400-second override for
    // the variant_calling class. 2 hours into a 4-hour budget is NOT
    // stale.
    std::fs::create_dir_all(pkg.join("policies")).unwrap();
    let profiles = serde_json::json!({
        "profiles": {
            "variant_calling": {
                "description": "long-running",
                "requirements": {"vcpus": 8, "memory_gb": 64, "storage_gb": 300},
                "ssm_timeout_secs": 14400,
            }
        },
        "default": {
            "description": "fallback",
            "requirements": {"vcpus": 2, "memory_gb": 8, "storage_gb": 50}
        }
    });
    std::fs::write(
        pkg.join("policies/compute-resource-policy.json"),
        serde_json::to_string_pretty(&profiles).unwrap(),
    )
    .unwrap();

    let responses = canned_run_iteration_success(0);
    let (bin_dir, _log) = install_shim(scratch.path(), &as_str_map(&responses));

    let _env = EnvGuard::new(&required_env());
    let _path = PathGuard::prepend(&bin_dir);

    // session-level timeout is tiny so this test exercises the per-stage
    // override path, not the fallback chain.
    let mut exec = make_executor(pkg.to_str().unwrap(), 60);
    provision_with_shim(&mut exec, &pkg);

    let started = chrono::Utc::now() - chrono::Duration::seconds(7200); // 2h ago
    let task = Task {
        kind: TaskKind::Computation,
        state: TaskState::Running {
            started_at: started.to_rfc3339(),
            remote: Some(RemoteExecution {
                backend: "aws".into(),
                instance_id: "i-shim-001".into(),
                instance_type: "r6i.4xlarge".into(),
                command_id: None,
                output_uri: None,
            }),
        },
        depends_on: vec![],
        assignee: Assignee::Agent,
        description: "variant calling".into(),
        spec: Some(serde_json::json!({
            "stage_class": "variant_calling",
            "task_id": "long-running-1",
        })),
        resolution: None,
        result_ref: None,
        resource_class: ResourceClass::CpuHeavy,
        requires_sme_review: false,

        required_artifacts: vec![],
        container: None,
        source_atom_id: None,
        safety: Default::default(),
    };
    let now = chrono::Utc::now().timestamp() as u64;
    assert!(
        !exec.is_task_stale(&task, now),
        "per-stage override (14400s) means 7200s is well within budget — not stale"
    );
}

#[test]
fn is_task_stale_uses_env_ssm_timeout_when_no_profile_override() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let scratch = tempfile::tempdir().unwrap();
    let pkg = scratch.path().join("pkg");
    seed_ready_dag(&pkg, "env-tune-1");

    // No policies/compute-resource-policy.json — the fallback chain
    // consults ECAA_AWS_SSM_TIMEOUT_SECS next. Set it to a large
    // value so an old-looking task still isn't stale on timestamp
    // alone; the SSM query is skipped because elapsed < timeout.
    let responses = canned_run_iteration_success(0);
    let (bin_dir, _log) = install_shim(scratch.path(), &as_str_map(&responses));

    let _env = EnvGuard::new(&required_env());
    let _path = PathGuard::prepend(&bin_dir);
    let _tmo = EnvGuard::new(&[("ECAA_AWS_SSM_TIMEOUT_SECS", "14400")]);

    let mut exec = make_executor(pkg.to_str().unwrap(), 60);
    provision_with_shim(&mut exec, &pkg);

    let started = chrono::Utc::now() - chrono::Duration::seconds(3600);
    let task = Task {
        kind: TaskKind::Computation,
        state: TaskState::Running {
            started_at: started.to_rfc3339(),
            remote: Some(RemoteExecution {
                backend: "aws".into(),
                instance_id: "i-shim-001".into(),
                instance_type: "r6i.2xlarge".into(),
                command_id: None,
                output_uri: None,
            }),
        },
        depends_on: vec![],
        assignee: Assignee::Agent,
        description: "whatever".into(),
        spec: Some(serde_json::json!({
            "stage_class": "alignment_quantification",
            "task_id": "env-tune-1",
        })),
        resolution: None,
        result_ref: None,
        resource_class: ResourceClass::CpuHeavy,
        requires_sme_review: false,

        required_artifacts: vec![],
        container: None,
        source_atom_id: None,
        safety: Default::default(),
    };
    let now = chrono::Utc::now().timestamp() as u64;
    assert!(
        !exec.is_task_stale(&task, now),
        "env-tuned 14400s timeout means 3600s elapsed is NOT stale"
    );
}

// Reference types to keep `BlockedRecord` import used — future PRs
// testing blocker cases will consume it. Silences unused-warning today.
#[allow(dead_code)]
fn _keep_blocked_record_used(_b: BlockedRecord) {}
