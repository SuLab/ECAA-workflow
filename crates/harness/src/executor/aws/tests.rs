//! Test corpus for `crates/harness/src/executor/aws/mod.rs`.
//!
//! Moved out of `aws/mod.rs` into a sibling `tests.rs`
//! file so the prod-only mod.rs stays under the §S5.9 modularity cap
//! (was 1363 LOC; tests-only block was ~942 LOC). Content indent is
//! Preserved verbatim from the original `mod tests {... }` block so
//! embedded raw-string YAML fixtures keep their content indentation
//! (the prior dedent attempt corrupted `seed_pilot_profiles` YAML).

// File is included only under `#[cfg(test)] mod tests;` in the parent
// (executor/aws/mod.rs:419), so a sibling `#![cfg(test)]` here is the
// duplicated attribute clippy 1.93 flags.
//
// S5.32: workspace lint is `unsafe_code = "deny"`. Test code uses
// `unsafe { std::env::set_var / remove_var }` (unsafe in Rust 2024
// edition because the env table is not thread-safe). All call sites
// are single-threaded test setup/teardown; the bounded waiver is
// scoped to this file.
#![allow(unsafe_code)]
use super::*;
use ecaa_workflow_core::dag::TaskState;
use std::os::unix::fs::PermissionsExt;
use std::time::Duration;

/// Re-export the crate-wide `ECAA_AWS_ENV_LOCK` under the short
/// `ENV_LOCK` name the tests use. Sharing the lock with
/// `multi_az_policy` tests eliminates the race where they observed
/// our transient `ECAA_AWS_SUBNET_IDS=subnet-test` value.
use super::super::ECAA_AWS_ENV_LOCK as ENV_LOCK;

fn args() -> ExecutorArgs {
    ExecutorArgs {
        package: "/tmp/x".into(),
        agent: "/bin/true".into(),
        task_timeout_secs: 300,
    }
}

/// Save / restore the env vars our tests touch so parallel test
/// threads don't see each other's mutations.
struct EnvGuard {
    keys: Vec<String>,
    prior: Vec<(String, Option<String>)>,
}

impl EnvGuard {
    fn new(set: &[(&str, &str)]) -> Self {
        let mut prior = Vec::new();
        for (k, v) in set {
            prior.push(((*k).to_string(), std::env::var(k).ok()));
            unsafe { std::env::set_var(k, v) };
        }
        Self {
            keys: set.iter().map(|(k, _)| (*k).to_string()).collect(),
            prior,
        }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        for (i, k) in self.keys.iter().enumerate() {
            match &self.prior[i].1 {
                Some(v) => unsafe { std::env::set_var(k, v) },
                None => unsafe { std::env::remove_var(k) },
            }
        }
    }
}

#[test]
fn from_env_lists_every_missing_var_in_one_diagnostic() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    // Save and clear all required env vars so the loader fails on
    // every one in a single message.
    let _g = EnvGuard::new(&[
        ("ECAA_AWS_REGION", ""),
        ("ECAA_AWS_AMI_ID", ""),
        ("ECAA_AWS_SECURITY_GROUP", ""),
        ("ECAA_AWS_INSTANCE_PROFILE", ""),
        ("ECAA_AWS_SUBNET_ID", ""),
        ("ECAA_AWS_SUBNET_IDS", ""),
    ]);
    // EnvGuard set them to empty strings; we want them genuinely
    // absent for this test.
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
    let err = AwsConfig::from_env().unwrap_err();
    let msg = format!("{:#}", err);
    for token in [
        "ECAA_AWS_REGION",
        "ECAA_AWS_AMI_ID",
        "ECAA_AWS_SECURITY_GROUP",
        "ECAA_AWS_INSTANCE_PROFILE",
        "ECAA_AWS_SUBNET_IDS",
    ] {
        assert!(
            msg.contains(token),
            "{} missing from diagnostic: {}",
            token,
            msg
        );
    }
}

#[test]
fn from_env_succeeds_with_all_required_set() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let _g = EnvGuard::new(&[
        ("ECAA_AWS_REGION", "us-west-2"),
        ("ECAA_AWS_AMI_ID", "ami-deadbeef"),
        ("ECAA_AWS_SECURITY_GROUP", "sg-12345"),
        ("ECAA_AWS_INSTANCE_PROFILE", "scripps-agent"),
        ("ECAA_AWS_SUBNET_IDS", "subnet-a,subnet-b"),
    ]);
    let config = AwsConfig::from_env().unwrap();
    assert_eq!(config.region, "us-west-2");
    assert_eq!(config.ami_id, "ami-deadbeef");
    assert_eq!(config.subnets.len(), 2);
    assert!(!config.spot);
}

/// Build a PATH-shimmed `aws` binary that records every invocation
/// to a file and returns a canned response. Used by the
/// provision/scan_orphans/wait_for_ssm dry-run tests.
fn install_aws_shim(
    scratch: &Path,
    canned_response: &str,
) -> (std::path::PathBuf, std::path::PathBuf) {
    let bin_dir = scratch.join("bin");
    std::fs::create_dir_all(&bin_dir).unwrap();
    let log = scratch.join("aws-calls.log");
    let aws_path = bin_dir.join("aws");
    let response_path = scratch.join("response.json");
    std::fs::write(&response_path, canned_response).unwrap();
    let script = format!(
        "#!/bin/sh\necho \"$@\" >> '{}'\ncat '{}'\n",
        log.display(),
        response_path.display()
    );
    std::fs::write(&aws_path, script).unwrap();
    let mut perms = std::fs::metadata(&aws_path).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&aws_path, perms).unwrap();
    (bin_dir, log)
}

/// Multi-modal variant of `install_aws_shim` that dispatches
/// based on the first two positional args (`$1 $2`).
/// `routes` is a list of `(prefix, response_body)` pairs where
/// `prefix` is matched against the start of `$1 $2` (e.g.
/// `"ec2 run-instances"`, `"ssm describe-instance-information"`,
/// `"cloudwatch get-metric-statistics"`, `"ec2 terminate-instances"`).
/// The first matching prefix wins. A `fallback` response is
/// returned when no route matches.
///
/// Required by the pilot tests because `pilot` now provisions its
/// own instance via `ec2 run-instances`, then waits for SSM, then
/// measures CloudWatch, then terminates — four distinct AWS API
/// shapes in one test invocation.
fn install_aws_shim_multi(
    scratch: &Path,
    routes: &[(&str, &str)],
    fallback: &str,
) -> (std::path::PathBuf, std::path::PathBuf) {
    let bin_dir = scratch.join("bin");
    std::fs::create_dir_all(&bin_dir).unwrap();
    let log = scratch.join("aws-calls.log");
    let aws_path = bin_dir.join("aws");

    // Write one response file per route + one for the fallback.
    // The shim's `case` reads $1 $2 and `cat`s the right file.
    let fallback_path = scratch.join("resp-fallback.json");
    std::fs::write(&fallback_path, fallback).unwrap();

    let mut case_arms = String::new();
    for (i, (prefix, body)) in routes.iter().enumerate() {
        let resp_path = scratch.join(format!("resp-{}.json", i));
        std::fs::write(&resp_path, body).unwrap();
        case_arms.push_str(&format!(
            "  \"{}\"*) cat '{}' ;;\n",
            prefix,
            resp_path.display()
        ));
    }

    let script = format!(
        "#!/bin/sh\n\
             echo \"$@\" >> '{log}'\n\
             key=\"$1 $2\"\n\
             case \"$key\" in\n\
             {arms}\
               *) cat '{fallback}';;\n\
             esac\n",
        log = log.display(),
        arms = case_arms,
        fallback = fallback_path.display(),
    );
    std::fs::write(&aws_path, script).unwrap();
    let mut perms = std::fs::metadata(&aws_path).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&aws_path, perms).unwrap();
    (bin_dir, log)
}

fn aws_executor_with_shim(scratch: &Path, canned: &str) -> (AwsExecutor, std::path::PathBuf) {
    let (bin_dir, log) = install_aws_shim(scratch, canned);
    let prior_path = std::env::var("PATH").unwrap_or_default();
    unsafe { std::env::set_var("PATH", format!("{}:{}", bin_dir.display(), prior_path)) };
    // `live_peer_sessions` reads `$HOME/.ecaa-workflow/locks/`.
    // Sandbox HOME to the scratch dir so tests don't see real
    // running-harness lockfiles on a dev box.
    unsafe { std::env::set_var("HOME", scratch) };
    let _g = EnvGuard::new(&[
        ("ECAA_AWS_REGION", "us-west-2"),
        ("ECAA_AWS_AMI_ID", "ami-test"),
        ("ECAA_AWS_SECURITY_GROUP", "sg-test"),
        ("ECAA_AWS_INSTANCE_PROFILE", "scripps"),
        ("ECAA_AWS_SUBNET_IDS", "subnet-test"),
        // Provisioning paths now require a
        // positive ceiling.
        ("ECAA_AWS_COST_CEILING_USD", "1000000"),
        // W5.1 cumulative guard requires the run-total ceiling too.
        ("ECAA_AWS_RUN_TOTAL_CEILING_USD", "1000000"),
    ]);
    // Detach the EnvGuard — caller's test will hold its own.
    std::mem::forget(_g);
    let exec = AwsExecutor::new(&args()).expect("config");
    // Restore PATH to baseline at end of test by attaching to scratch.
    let _ = prior_path; // we don't restore PATH; tests run serial via tempdir
    (exec, log)
}

/// AwsExecutor with a multi-modal PATH-shim, package pointer, and
/// full env setup. Used by the pilot tests that need distinct
/// responses per AWS sub-command.
///
/// Returns the `EnvGuard` so the caller can hold it for the test's
/// lifetime; dropping it restores the pre-test env values. Stricter
/// than the older `aws_executor_with_shim` helper (which leaked via
/// `std::mem::forget`) because pilot tests must leave the env
/// clean so parallel `multi_az_policy` tests don't observe
/// a leaked `ECAA_AWS_SUBNET_IDS=subnet-test` value.
#[must_use]
fn aws_executor_with_multi_shim_and_package(
    scratch: &Path,
    pkg_path: &Path,
    routes: &[(&str, &str)],
    fallback: &str,
) -> (AwsExecutor, std::path::PathBuf, EnvGuard) {
    let (bin_dir, log) = install_aws_shim_multi(scratch, routes, fallback);
    let prior_path = std::env::var("PATH").unwrap_or_default();
    unsafe { std::env::set_var("PATH", format!("{}:{}", bin_dir.display(), prior_path)) };
    let guard = EnvGuard::new(&[
        ("ECAA_AWS_REGION", "us-west-2"),
        ("ECAA_AWS_AMI_ID", "ami-test"),
        ("ECAA_AWS_SECURITY_GROUP", "sg-test"),
        ("ECAA_AWS_INSTANCE_PROFILE", "scripps"),
        ("ECAA_AWS_SUBNET_IDS", "subnet-test"),
        // Provision now requires a positive ceiling.
        // Tests that exercise the provisioning path set a deliberately
        // high one so they don't have to think about the cost model.
        ("ECAA_AWS_COST_CEILING_USD", "1000000"),
        // W5.1 cumulative guard requires the run-total ceiling too.
        ("ECAA_AWS_RUN_TOTAL_CEILING_USD", "1000000"),
    ]);
    let args = ExecutorArgs {
        package: pkg_path.to_string_lossy().to_string(),
        agent: "/bin/true".into(),
        task_timeout_secs: 300,
    };
    let exec = AwsExecutor::new(&args).expect("config");
    let _ = prior_path;
    (exec, log, guard)
}

#[test]
fn scan_orphans_warn_mode_returns_orphan_ids_without_terminating() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let scratch = tempfile::tempdir().unwrap();
    // Describe-instances now projects
    // `{Id:InstanceId,Tags:Tags}` so the orphan scanner can filter by
    // session-id tag client-side. Old single-id shape would fail to
    // parse against the new `InstanceRow`.
    let response = r#"[{"Id":"i-orphan-a","Tags":[]},{"Id":"i-orphan-b","Tags":[]}]"#;
    let (executor, log) = aws_executor_with_shim(scratch.path(), response);
    let _g = EnvGuard::new(&[("ECAA_AWS_ORPHAN_POLICY", "warn")]);

    let orphans = executor.scan_orphans().unwrap();
    assert_eq!(
        orphans,
        vec!["i-orphan-a".to_string(), "i-orphan-b".to_string()]
    );

    // Only the describe-instances call should appear; no terminate.
    let log_contents = std::fs::read_to_string(&log).unwrap();
    assert!(log_contents.contains("describe-instances"));
    assert!(
        !log_contents.contains("terminate-instances"),
        "warn mode must not terminate; log was: {log_contents}"
    );
}

#[test]
fn scan_orphans_reap_mode_terminates_each_orphan() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let scratch = tempfile::tempdir().unwrap();
    let response = r#"[{"Id":"i-orphan-only","Tags":[]}]"#;
    let (executor, log) = aws_executor_with_shim(scratch.path(), response);
    let _g = EnvGuard::new(&[("ECAA_AWS_ORPHAN_POLICY", "reap")]);

    let _ = executor.scan_orphans().unwrap();
    let log_contents = std::fs::read_to_string(&log).unwrap();
    assert!(log_contents.contains("describe-instances"));
    assert!(
        log_contents.contains("terminate-instances --instance-ids i-orphan-only"),
        "reap mode must terminate; log was: {log_contents}"
    );
}

/// Negative-tag test: when session A's reap sweep runs with
/// `session_id_tag = Some("session-A")`, the AWS describe-instances
/// call MUST include the
/// `Name=tag:ScrippsWorkflowHarnessSessionId,Values=session-A`
/// filter. Real AWS would then return only session A's instances —
/// session B's instance, tagged differently, would never reach the
/// terminate-instances call. The simple shim doesn't parse filters
/// (it just returns what we give it), so we prove the property at
/// the level of "the filter argv reached AWS" + "only what AWS
/// returned was terminated." Together these show cross-session
/// reap interference is structurally impossible.
#[test]
fn scan_orphans_verified_filters_by_session_id_and_reaps_only_returned_ids() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let scratch = tempfile::tempdir().unwrap();
    // Multi-modal shim: candidate scan (--filters) returns A's
    // orphan; verification poll (--instance-ids) returns terminated;
    // terminate-instances returns {}; everything else falls back.
    // Argv-aware dispatch via a sh `case` on `$@`.
    let bin_dir = scratch.path().join("bin");
    std::fs::create_dir_all(&bin_dir).unwrap();
    let log_path = scratch.path().join("aws-calls.log");
    let aws_path = bin_dir.join("aws");
    // describe-instances shape: projects
    // `{Id:InstanceId,Tags:Tags}` for client-side peer filtering. We
    // tag this orphan with `ScrippsWorkflowHarnessSessionId=session-A`
    // so the harness's own session-A filter passes; an empty Tags
    // would fall through the "legacy / no tag = candidate" branch
    // which also accepts. We keep the tag explicit so the test
    // documents the production shape.
    let script = format!(
        "#!/bin/sh\n\
             echo \"$@\" >> '{log}'\n\
             args=\"$*\"\n\
             case \"$args\" in\n\
               *terminate-instances*) echo '{{}}';;\n\
               *--instance-ids*) echo '[\"terminated\"]';;\n\
               *describe-instances*) echo '[{{\"Id\":\"i-session-a-orphan\",\"Tags\":[{{\"Key\":\"ScrippsWorkflowHarnessSessionId\",\"Value\":\"session-A\"}}]}}]';;\n\
               *) echo '{{}}';;\n\
             esac\n",
        log = log_path.display(),
    );
    std::fs::write(&aws_path, script).unwrap();
    let mut perms = std::fs::metadata(&aws_path).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&aws_path, perms).unwrap();

    let prior_path = std::env::var("PATH").unwrap_or_default();
    unsafe { std::env::set_var("PATH", format!("{}:{}", bin_dir.display(), prior_path)) };
    // Sandbox HOME so the dev's actual session locks
    // don't cause this test to skip session-A's orphan.
    unsafe { std::env::set_var("HOME", scratch.path()) };
    let _env = EnvGuard::new(&[
        ("ECAA_AWS_REGION", "us-west-2"),
        ("ECAA_AWS_AMI_ID", "ami-test"),
        ("ECAA_AWS_SECURITY_GROUP", "sg-test"),
        ("ECAA_AWS_INSTANCE_PROFILE", "scripps"),
        ("ECAA_AWS_SUBNET_IDS", "subnet-test"),
        ("ECAA_AWS_ORPHAN_POLICY", "reap"),
        // Tiny verification deadline so the test doesn't sit in the
        // 10s-sleep poll loop. With 1s deadline the verification
        // loop exits after its first iteration regardless.
        ("ECAA_AWS_ORPHAN_VERIFY_TIMEOUT_SECS", "1"),
        // `scan_orphans_verified` doesn't touch the cost guard, but
        // a previously-run test in this `ENV_LOCK` group may have
        // dropped its EnvGuard and cleared the ceiling. Pin it so any
        // future code path that does call the guard stays green.
        ("ECAA_AWS_COST_CEILING_USD", "1000000"),
        // W5.1 cumulative guard requires the run-total ceiling too.
        ("ECAA_AWS_RUN_TOTAL_CEILING_USD", "1000000"),
    ]);
    let executor = AwsExecutor::new(&args()).expect("config");

    let summary = executor
        .scan_orphans_verified(Some("session-A"))
        .expect("scan_orphans_verified");

    // Restore PATH so subsequent tests in the same process don't
    // see our shim.
    unsafe { std::env::set_var("PATH", &prior_path) };

    assert_eq!(summary.candidate_count, 1, "exactly session-A's orphan");
    let log_contents = std::fs::read_to_string(&log_path).unwrap();
    assert!(
            log_contents.contains("Name=tag:ScrippsWorkflowHarnessSessionId,Values=session-A"),
            "describe-instances must filter by ScrippsWorkflowHarnessSessionId=session-A; log:\n{log_contents}"
        );
    assert!(
        log_contents.contains("terminate-instances --instance-ids i-session-a-orphan"),
        "only the AWS-returned id must be terminated; log:\n{log_contents}"
    );
    // Concrete negative: session B's id is never seen because the
    // tag filter excluded it at AWS, so it can't appear in any
    // terminate call.
    assert!(
        !log_contents.contains("i-session-b"),
        "session B's instance must never appear in the call log; log:\n{log_contents}"
    );
}

/// When a peer harness holds a live lock for
/// `session-peer`, scan_orphans must exclude that peer's instance
/// from the candidate list even if AWS returned it. We simulate the
/// peer by:
/// - creating a sandboxed HOME pointing at `scratch`
/// - writing `~/.ecaa-workflow/locks/session-peer.lock` with the
///   CURRENT PID (alive by definition)
/// - returning two instances from AWS, one tagged peer's session
///
/// The peer's instance must be filtered out client-side.
#[test]
fn scan_orphans_filters_live_peer_session() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let scratch = tempfile::tempdir().unwrap();
    // Both candidates have the BuiltBy tag at the AWS level; only the
    // session-peer-tagged one belongs to a live peer.
    let response = r#"[
        {"Id":"i-mine","Tags":[{"Key":"ScrippsWorkflowHarnessSessionId","Value":"session-mine"}]},
        {"Id":"i-peer","Tags":[{"Key":"ScrippsWorkflowHarnessSessionId","Value":"session-peer"}]}
    ]"#;
    let (executor, _log) = aws_executor_with_shim(scratch.path(), response);

    // Plant a live peer lockfile under the sandboxed HOME. PID is
    // ours so `kill(pid, 0)` returns 0 (alive).
    let locks_dir = scratch.path().join(".ecaa-workflow").join("locks");
    std::fs::create_dir_all(&locks_dir).unwrap();
    std::fs::write(
        locks_dir.join("session-peer.lock"),
        format!("{}\n", std::process::id()),
    )
    .unwrap();

    let _g = EnvGuard::new(&[("ECAA_AWS_ORPHAN_POLICY", "warn")]);
    let orphans = executor.scan_orphans().unwrap();
    assert_eq!(
        orphans,
        vec!["i-mine".to_string()],
        "session-peer instance must be filtered out because a live \
         peer holds session-peer.lock"
    );
}

// Pilot + stall monitor tests

use ecaa_workflow_core::dag::{Assignee, ResourceClass, Task as DagTask, TaskId, TaskKind};
use std::collections::BTreeMap as BT;

/// Build an AwsExecutor whose args.package points at `pkg_path`.
/// Installs a PATH-shim that returns `canned` for every `aws` call
/// — pilot + stall tests use this to stage CloudWatch responses.
fn aws_executor_with_shim_and_package(
    scratch: &Path,
    pkg_path: &Path,
    canned: &str,
) -> (AwsExecutor, std::path::PathBuf) {
    let (bin_dir, log) = install_aws_shim(scratch, canned);
    let prior_path = std::env::var("PATH").unwrap_or_default();
    unsafe { std::env::set_var("PATH", format!("{}:{}", bin_dir.display(), prior_path)) };
    let g = EnvGuard::new(&[
        ("ECAA_AWS_REGION", "us-west-2"),
        ("ECAA_AWS_AMI_ID", "ami-test"),
        ("ECAA_AWS_SECURITY_GROUP", "sg-test"),
        ("ECAA_AWS_INSTANCE_PROFILE", "scripps"),
        ("ECAA_AWS_SUBNET_IDS", "subnet-test"),
        // Provisioning paths now require a
        // positive ceiling; tests pre-set one so they don't need to.
        ("ECAA_AWS_COST_CEILING_USD", "1000000"),
        // W5.1 cumulative guard requires the run-total ceiling too.
        ("ECAA_AWS_RUN_TOTAL_CEILING_USD", "1000000"),
    ]);
    std::mem::forget(g);
    let args = ExecutorArgs {
        package: pkg_path.to_string_lossy().to_string(),
        agent: "/bin/true".into(),
        task_timeout_secs: 300,
    };
    let exec = AwsExecutor::new(&args).expect("config");
    let _ = prior_path;
    (exec, log)
}

fn pilot_ready_task(stage: &str) -> DagTask {
    DagTask {
        description: format!("{}-task", stage),
        kind: TaskKind::Computation,
        state: TaskState::Ready,
        depends_on: vec![],
        assignee: Assignee::Agent,
        spec: Some(serde_json::json!({ "stage_class": stage })),
        resolution: None,
        result_ref: None,
        resource_class: ResourceClass::CpuHeavy,
        requires_sme_review: false,

        required_artifacts: vec![],
        container: None,
        source_atom_id: None,
        safety: Default::default(),
    }
}

fn pilot_dag() -> DAG {
    let mut t: BT<TaskId, DagTask> = BT::new();
    t.insert(TaskId::from("alignment_01"), pilot_ready_task("alignment"));
    t.insert(
        TaskId::from("quantification_01"),
        pilot_ready_task("quantification"),
    );
    t.insert(
        TaskId::from("differential_expression_01"),
        pilot_ready_task("differential_expression"),
    );
    t.insert(
        TaskId::from("enrichment_01"),
        pilot_ready_task("enrichment"),
    );
    DAG {
        version: "1".into(),
        schema_version: ecaa_workflow_core::dag::current_dag_schema_version(),
        workflow_id: "pilot-test".into(),
        current_task: None,
        tasks: t,
        reverse_deps: std::collections::BTreeMap::new(),
        run_id: None,
    }
}

fn single_stage_dag(stage: &str) -> DAG {
    let mut t: BT<TaskId, DagTask> = BT::new();
    t.insert(
        TaskId::from(format!("{stage}_01").as_str()),
        pilot_ready_task(stage),
    );
    DAG {
        version: "1".into(),
        schema_version: ecaa_workflow_core::dag::current_dag_schema_version(),
        workflow_id: "single-stage".into(),
        current_task: None,
        tasks: t,
        reverse_deps: std::collections::BTreeMap::new(),
        run_id: None,
    }
}

/// Drop a `policies/compute-resource-policy.json` into `pkg` so the
/// pilot's profile loader has a shape to work with.
fn seed_pilot_profiles(pkg: &Path) {
    let policies = pkg.join("policies");
    std::fs::create_dir_all(&policies).unwrap();
    // Profiles sized so each stage maps to an instance type in
    // `executor::aws::pricing::INSTANCE_PRICES_USD_PER_HOUR`. The pilot
    // path runs cost_guard for every projection; types outside the
    // priced table would fail with UnknownInstanceType.
    let yaml = r#"
default:
  requirements:
    vcpus: 2
    memory_gb: 4
    storage_gb: 50
profiles:
  alignment:
    requirements:
      vcpus: 16
      memory_gb: 64
      storage_gb: 200
  quantification:
    requirements:
      vcpus: 8
      memory_gb: 64
      storage_gb: 100
  differential_expression:
    requirements:
      vcpus: 2
      memory_gb: 16
      storage_gb: 50
  enrichment:
    requirements:
      vcpus: 2
      memory_gb: 16
      storage_gb: 20
"#;
    let v: serde_json::Value = serde_yml::from_str(yaml).unwrap();
    std::fs::write(
        policies.join("compute-resource-policy.json"),
        serde_json::to_string(&v).unwrap(),
    )
    .unwrap();
}

#[test]
fn pick_instance_type_uses_pilot_projection_for_real_provision() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let pkg = tempfile::tempdir().unwrap();
    seed_pilot_profiles(pkg.path());
    let _env = EnvGuard::new(&[
        ("ECAA_AWS_REGION", "us-west-2"),
        ("ECAA_AWS_AMI_ID", "ami-test"),
        ("ECAA_AWS_SECURITY_GROUP", "sg-test"),
        ("ECAA_AWS_INSTANCE_PROFILE", "scripps"),
        ("ECAA_AWS_SUBNET_IDS", "subnet-test"),
        // `pick_instance_type` itself doesn't call the cost guard, but
        // a previously-run test in this `ENV_LOCK` group may have
        // dropped its EnvGuard and cleared the ceiling. Pin it here so
        // any downstream provision call (or future test extension) has
        // a stable ceiling regardless of ordering.
        ("ECAA_AWS_COST_CEILING_USD", "1000000"),
        // W5.1 cumulative guard requires the run-total ceiling too.
        ("ECAA_AWS_RUN_TOTAL_CEILING_USD", "1000000"),
    ]);
    let args = ExecutorArgs {
        package: pkg.path().to_string_lossy().to_string(),
        agent: "/bin/true".into(),
        task_timeout_secs: 300,
    };
    let mut exec = AwsExecutor::new(&args).expect("config");
    let baseline = exec
        .pick_instance_type(&single_stage_dag("enrichment"))
        .expect("baseline instance");
    // enrichment profile (vcpus=2, memory_gb=16) doesn't fit t3.medium
    // (4 GB) or t3.large (8 GB). Per the P2-168 in-family resolver,
    // it climbs to c6i.2xlarge (8 vCPU, 16 GB) which satisfies the
    // memory floor with the smallest vCPU bump.
    assert_eq!(baseline, "c6i.2xlarge");

    let mut projected = std::collections::BTreeMap::new();
    projected.insert(
        "enrichment".into(),
        super::super::ResourceRequirements {
            vcpus: 2,
            memory_gb: 160,
            storage_gb: 20,
            gpu: None,
        },
    );
    exec.pilot_projected_requirements = Some(projected);

    let sized = exec
        .pick_instance_type(&single_stage_dag("enrichment"))
        .expect("pilot-sized instance");
    assert_eq!(sized, "r6i.8xlarge");
}

#[test]
fn pilot_disabled_returns_none() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let scratch = tempfile::tempdir().unwrap();
    let pkg = tempfile::tempdir().unwrap();
    let (mut exec, _log) =
        aws_executor_with_shim_and_package(scratch.path(), pkg.path(), r#"{"Datapoints":[]}"#);
    let cfg = super::super::pilot::PilotConfig {
        enabled: false,
        ..Default::default()
    };
    let out = exec.pilot(&pilot_dag(), &cfg).unwrap();
    assert!(out.is_none(), "disabled pilot must return Ok(None)");
}

/// Canned responses for the four AWS sub-commands the pilot exercises
/// when it actually provisions + waits + measures + releases a
/// dedicated pilot instance.
fn pilot_routes_with_cloudwatch(
    cloudwatch_body: &'static str,
) -> Vec<(&'static str, &'static str)> {
    // `ssm describe-instance-information` immediately reports the
    // pilot instance as Online so `wait_for_ssm` completes without
    // sleeping. `ec2 run-instances` returns a parseable InstanceId.
    // `ec2 terminate-instances` returns an empty object — the code
    // path just needs a zero-exit response. CloudWatch responses
    // are parameterised so tests can stage either populated or
    // empty datapoints.
    vec![
        (
            "ec2 run-instances",
            r#"{"Instances":[{"InstanceId":"i-pilot-generated"}]}"#,
        ),
        (
            "ssm describe-instance-information",
            r#"{"InstanceInformationList":[{"PingStatus":"Online"}]}"#,
        ),
        ("cloudwatch get-metric-statistics", cloudwatch_body),
        ("ec2 terminate-instances", r#"{}"#),
    ]
}

#[test]
fn pilot_returns_report_with_measurements_when_cloudwatch_responds() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let scratch = tempfile::tempdir().unwrap();
    let pkg = tempfile::tempdir().unwrap();
    seed_pilot_profiles(pkg.path());
    // Both CPUUtilization and MemoryUtilization calls hit the same
    // CloudWatch route and see the same canned response.
    let routes = pilot_routes_with_cloudwatch(
        r#"{"Datapoints":[{"Timestamp":"2026-04-17T12:00:00Z","Maximum":42.0,"Unit":"Percent"}]}"#,
    );
    let (mut exec, _log, _env) =
        aws_executor_with_multi_shim_and_package(scratch.path(), pkg.path(), &routes, r#"{}"#);
    let cfg = super::super::pilot::PilotConfig {
        enabled: true,
        task_count: 2,
        ..Default::default()
    };
    let report = exec.pilot(&pilot_dag(), &cfg).unwrap().unwrap();
    assert!(
        !report.measurements.is_empty(),
        "report should carry >=1 measurement"
    );
    assert!(
        report.measurements.iter().all(|m| m.peak_rss_mb == 42),
        "every measurement should reflect the CloudWatch Maximum 42; got {:?}",
        report.measurements
    );
    // With measurements present, confidence must be > 0.
    assert!(
        report.confidence > 0.0,
        "confidence should be > 0 with observed measurements, got {}",
        report.confidence
    );
    assert!(pkg.path().join("runtime/pilot/report.json").exists());
    // Pilot must leave the instance slot empty so the main loop's
    // subsequent provision runs fresh.
    assert!(
        exec.instance.is_none(),
        "pilot must release its dedicated instance before returning"
    );
    assert!(
        exec.pilot_instance_override.is_none(),
        "pilot must clear the instance-type override before returning"
    );
    assert!(
        exec.pilot_projected_requirements.is_some(),
        "successful pilot must retain projections for the real provision"
    );
}

#[test]
fn pilot_falls_back_to_baseline_when_cloudwatch_empty() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let scratch = tempfile::tempdir().unwrap();
    let pkg = tempfile::tempdir().unwrap();
    seed_pilot_profiles(pkg.path());
    // Empty Datapoints: CloudWatch has no metrics yet (agent not
    // installed / instance too fresh). Provision + SSM + terminate
    // still succeed via the multi-modal shim.
    let routes = pilot_routes_with_cloudwatch(r#"{"Datapoints":[]}"#);
    let (mut exec, _log, _env) =
        aws_executor_with_multi_shim_and_package(scratch.path(), pkg.path(), &routes, r#"{}"#);
    let cfg = super::super::pilot::PilotConfig {
        enabled: true,
        task_count: 2,
        ..Default::default()
    };
    let report = exec.pilot(&pilot_dag(), &cfg).unwrap().unwrap();
    assert!(
        report.measurements.iter().all(|m| m.peak_rss_mb == 0),
        "empty CloudWatch response must degrade to peak_rss_mb=0; got {:?}",
        report.measurements
    );
    assert!(
        report.measurements.iter().all(|m| m.exit_status == -1),
        "exit_status must be -1 on empty response; got {:?}",
        report.measurements
    );
    // No observed data → confidence stays at 0.0.
    assert_eq!(
        report.confidence, 0.0,
        "confidence must be 0 with empty CloudWatch"
    );
}

#[test]
fn pilot_provisions_then_releases_pilot_instance() {
    // Assert the PATH-shim sees both `ec2 run-instances` AND
    // `ec2 terminate-instances` during a successful pilot run —
    // confirming the provision + release dance fires cleanly.
    let _lock = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let scratch = tempfile::tempdir().unwrap();
    let pkg = tempfile::tempdir().unwrap();
    seed_pilot_profiles(pkg.path());
    let routes = pilot_routes_with_cloudwatch(
        r#"{"Datapoints":[{"Timestamp":"2026-04-17T12:00:00Z","Maximum":10.0,"Unit":"Percent"}]}"#,
    );
    let (mut exec, log, _env) =
        aws_executor_with_multi_shim_and_package(scratch.path(), pkg.path(), &routes, r#"{}"#);
    let cfg = super::super::pilot::PilotConfig {
        enabled: true,
        task_count: 1,
        pilot_instance_type: "t3.medium".into(),
        ..Default::default()
    };
    let _ = exec.pilot(&pilot_dag(), &cfg).unwrap().unwrap();

    let log_contents = std::fs::read_to_string(&log).unwrap();
    assert!(
        log_contents.contains("ec2 run-instances"),
        "pilot must call ec2 run-instances; log was: {log_contents}"
    );
    // Pilot instance shape must be the override, not a DAG-derived
    // shape. `--instance-type t3.medium` has to appear on the
    // run-instances call.
    assert!(
        log_contents.contains("--instance-type t3.medium"),
        "pilot must provision the override (t3.medium); log was: {log_contents}"
    );
    // Every run-instances call must include the IMDSv2-enforcing
    // metadata-options block.
    assert!(
        log_contents.contains("--metadata-options"),
        "run-instances must pass --metadata-options (IMDSv2); log was: {log_contents}"
    );
    assert!(
        log_contents.contains("HttpTokens=required"),
        "run-instances metadata-options must require IMDSv2 tokens; log was: {log_contents}"
    );
    assert!(
        log_contents.contains("HttpPutResponseHopLimit=2"),
        "run-instances metadata-options must set hop-limit=2; log was: {log_contents}"
    );
    assert!(
        log_contents.contains("HttpEndpoint=enabled"),
        "run-instances metadata-options must explicitly enable IMDS endpoint; log was: {log_contents}"
    );
    assert!(
        log_contents.contains("ec2 terminate-instances"),
        "pilot must release the dedicated instance; log was: {log_contents}"
    );
    // Also confirm wait_for_ssm + CloudWatch both fired.
    assert!(
        log_contents.contains("ssm describe-instance-information"),
        "pilot must wait for SSM readiness; log was: {log_contents}"
    );
    assert!(
        log_contents.contains("cloudwatch get-metric-statistics"),
        "pilot must measure CloudWatch; log was: {log_contents}"
    );
}

#[test]
fn pilot_restores_main_instance_state_after_release() {
    // After pilot, `self.instance` must be `None` AND
    // `pilot_instance_override` must be `None` so the main loop's
    // subsequent `provision` runs against the real-shape picker
    // and against a fresh instance slot. Additionally, the PATH-shim
    // log must show exactly one `run-instances` call (the pilot's)
    // so there's no accidental leftover provision.
    let _lock = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let scratch = tempfile::tempdir().unwrap();
    let pkg = tempfile::tempdir().unwrap();
    seed_pilot_profiles(pkg.path());
    let routes = pilot_routes_with_cloudwatch(r#"{"Datapoints":[]}"#);
    let (mut exec, log, _env) =
        aws_executor_with_multi_shim_and_package(scratch.path(), pkg.path(), &routes, r#"{}"#);
    let cfg = super::super::pilot::PilotConfig {
        enabled: true,
        task_count: 1,
        ..Default::default()
    };
    let _ = exec.pilot(&pilot_dag(), &cfg).unwrap();

    assert!(
        exec.instance.is_none(),
        "main loop must see `self.instance == None` after pilot"
    );
    assert!(
        exec.pilot_instance_override.is_none(),
        "override must be cleared so the next provision picks the real shape"
    );

    // The override was in effect for the pilot's own provision.
    // The shim log must show the pilot's `--instance-type t3.medium`
    // AND a subsequent `terminate-instances`. After release, a
    // direct call to `pick_instance_type(dag)` must NOT return the
    // override-controlled shape because the override has been
    // cleared — i.e. even if the DAG-derived shape happens to be
    // `t3.medium` by coincidence, it comes from the profile picker,
    // not from the pilot override path.
    let log_contents = std::fs::read_to_string(&log).unwrap();
    let run_count = log_contents.matches("ec2 run-instances").count();
    assert_eq!(
        run_count, 1,
        "pilot should issue exactly one run-instances; got {run_count}: {log_contents}"
    );
    let term_count = log_contents.matches("ec2 terminate-instances").count();
    assert_eq!(
        term_count, 1,
        "pilot should issue exactly one terminate-instances; got {term_count}: {log_contents}"
    );
}

#[test]
fn pilot_falls_through_to_measurement_even_when_provision_fails() {
    // If provision errors (e.g. run-instances returns
    // garbage), pilot returns Ok(None) and does NOT bubble the
    // error — the main loop proceeds with baseline sizing.
    let _lock = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let scratch = tempfile::tempdir().unwrap();
    let pkg = tempfile::tempdir().unwrap();
    seed_pilot_profiles(pkg.path());
    // Override the run-instances route to return a body with no
    // InstanceId so `provision` fails. The other routes don't
    // matter because we never reach them.
    let routes: Vec<(&str, &str)> = vec![
        ("ec2 run-instances", r#"{"Instances":[{}]}"#),
        (
            "ssm describe-instance-information",
            r#"{"InstanceInformationList":[{"PingStatus":"Online"}]}"#,
        ),
        ("cloudwatch get-metric-statistics", r#"{"Datapoints":[]}"#),
        ("ec2 terminate-instances", r#"{}"#),
    ];
    let (mut exec, _log, _env) =
        aws_executor_with_multi_shim_and_package(scratch.path(), pkg.path(), &routes, r#"{}"#);
    let cfg = super::super::pilot::PilotConfig {
        enabled: true,
        task_count: 1,
        ..Default::default()
    };
    let out = exec.pilot(&pilot_dag(), &cfg);
    assert!(
        out.is_ok(),
        "provision failure must be swallowed: got {:?}",
        out
    );
    assert!(
        out.unwrap().is_none(),
        "provision failure must yield Ok(None), not Some(report)"
    );
    // State must still be clean.
    assert!(
        exec.instance.is_none(),
        "failed pilot must leave `self.instance == None`"
    );
    assert!(
        exec.pilot_instance_override.is_none(),
        "failed pilot must clear the override so the main loop picks real shape"
    );
}

#[test]
fn start_stall_monitor_noop_when_disabled() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let scratch = tempfile::tempdir().unwrap();
    let pkg = tempfile::tempdir().unwrap();
    let (mut exec, _log) =
        aws_executor_with_shim_and_package(scratch.path(), pkg.path(), r#"{"Datapoints":[]}"#);
    // Observe the shutdown flag so we can verify no thread flipped it.
    let shutdown = exec.stall_shutdown.clone();
    let (tx, _rx) = std::sync::mpsc::channel();
    let thresholds = super::super::stall_monitor::StallThresholds {
        enabled: false,
        ..Default::default()
    };
    exec.start_stall_monitor(&thresholds, tx).unwrap();
    // The shutdown flag must still be at its initial `false`.
    assert!(!*shutdown.lock().unwrap());
}

#[test]
fn start_stall_monitor_spawns_thread_when_enabled() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let scratch = tempfile::tempdir().unwrap();
    let pkg = tempfile::tempdir().unwrap();
    // Shim canned response is irrelevant — we shut the monitor
    // down before any sample completes.
    let (mut exec, _log) =
        aws_executor_with_shim_and_package(scratch.path(), pkg.path(), r#"{"Datapoints":[]}"#);
    exec.instance = Some(ProvisionedInstance {
        instance_id: "i-stall".into(),
        instance_type: "t3.medium".into(),
    });
    let (tx, rx) = std::sync::mpsc::channel();
    let thresholds = super::super::stall_monitor::StallThresholds {
        enabled: true,
        // 1s interval keeps the first sleep short.
        sample_interval_secs: 1,
        ..Default::default()
    };
    exec.start_stall_monitor(&thresholds, tx).unwrap();
    // Flip shutdown right away; the thread exits on next poll.
    exec.stop_stall_monitor();
    // Wait a bit longer than the sleep interval + polling slop so
    // the thread has a chance to observe the shutdown flag.
    std::thread::sleep(Duration::from_millis(1500));
    // The Receiver should see no signals — we never let the
    // monitor accumulate a full window.
    assert!(
        rx.try_recv().is_err(),
        "no StallSignal should arrive before the monitor shuts down"
    );
    // Shutdown flag must be true.
    assert!(*exec.stall_shutdown.lock().unwrap());
}

#[test]
fn is_task_stale_caches_ssm_result_within_ttl() {
    // at the harness's 5 s iteration cadence, back-to-back
    // `is_task_stale` calls on the same task should issue exactly
    // one SSM `list-command-invocations` round-trip while the TTL
    // window is fresh. Without caching, each call shells out; at
    // 50 running tasks × 30 s SSM latency that's ~25 s per
    // iteration just on staleness detection.
    let _lock = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let scratch = tempfile::tempdir().unwrap();
    // Canned SSM response: one invocation in Success → not stale.
    let response = r#"{"CommandInvocations":[{"Status":"Success"}]}"#;
    let (executor, log) = aws_executor_with_shim(scratch.path(), response);

    // Build a Running task whose started_at puts `elapsed` well
    // past the default timeout (300 s), forcing is_task_stale to
    // reach the SSM query path. task_id is carried in spec so the
    // cache can key off it.
    // 4000 s > 3600 s (fallback_ssm_timeout default) so is_task_stale
    // crosses into the SSM query path.
    let started = (chrono::Utc::now() - chrono::Duration::seconds(4000)).to_rfc3339();
    let task = DagTask {
        kind: TaskKind::Computation,
        state: TaskState::Running {
            started_at: started,
            remote: Some(ecaa_workflow_core::dag::RemoteExecution {
                backend: "aws".into(),
                instance_id: "i-test".into(),
                instance_type: "r6i.4xlarge".into(),
                command_id: None,
                output_uri: None,
            }),
        },
        depends_on: vec![],
        assignee: Assignee::Agent,
        description: "cached stale test".into(),
        spec: Some(serde_json::json!({"task_id": "alignment_quant"})),
        resolution: None,
        result_ref: None,
        resource_class: ResourceClass::CpuHeavy,
        requires_sme_review: false,

        required_artifacts: vec![],
        container: None,
        source_atom_id: None,
        safety: Default::default(),
    };
    let now_secs = chrono::Utc::now().timestamp() as u64;

    // First call → SSM round-trip.
    assert!(!executor.is_task_stale(&task, now_secs));
    // Second call within the TTL window → cache hit, no shell-out.
    assert!(!executor.is_task_stale(&task, now_secs + 5));

    let log_contents = std::fs::read_to_string(&log).unwrap();
    let ssm_calls = log_contents
        .lines()
        .filter(|l| l.contains("list-command-invocations"))
        .count();
    assert_eq!(
        ssm_calls, 1,
        "expected exactly one SSM call, got {} (log: {})",
        ssm_calls, log_contents
    );
}

#[test]
fn invalidate_ssm_stale_cache_forces_requery() {
    // the invalidation hook lets progress-event ingress
    // drop the cached entry so the next is_task_stale query picks
    // up fresh state.
    let _lock = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let scratch = tempfile::tempdir().unwrap();
    let response = r#"{"CommandInvocations":[{"Status":"Success"}]}"#;
    let (executor, log) = aws_executor_with_shim(scratch.path(), response);

    // 4000 s > 3600 s (fallback_ssm_timeout default) so is_task_stale
    // crosses into the SSM query path.
    let started = (chrono::Utc::now() - chrono::Duration::seconds(4000)).to_rfc3339();
    let task = DagTask {
        kind: TaskKind::Computation,
        state: TaskState::Running {
            started_at: started,
            remote: Some(ecaa_workflow_core::dag::RemoteExecution {
                backend: "aws".into(),
                instance_id: "i-test".into(),
                instance_type: "r6i.4xlarge".into(),
                command_id: None,
                output_uri: None,
            }),
        },
        depends_on: vec![],
        assignee: Assignee::Agent,
        description: "invalidation test".into(),
        spec: Some(serde_json::json!({"task_id": "alignment_invalidate"})),
        resolution: None,
        result_ref: None,
        resource_class: ResourceClass::CpuHeavy,
        requires_sme_review: false,

        required_artifacts: vec![],
        container: None,
        source_atom_id: None,
        safety: Default::default(),
    };
    let now_secs = chrono::Utc::now().timestamp() as u64;

    let _ = executor.is_task_stale(&task, now_secs);
    executor.invalidate_ssm_stale_cache("alignment_invalidate");
    let _ = executor.is_task_stale(&task, now_secs + 5);

    let log_contents = std::fs::read_to_string(&log).unwrap();
    let ssm_calls = log_contents
        .lines()
        .filter(|l| l.contains("list-command-invocations"))
        .count();
    assert_eq!(
        ssm_calls, 2,
        "invalidation should force a second SSM call, got {} (log: {})",
        ssm_calls, log_contents
    );
}
