//! Dispatch-time safety policy enforcement.
//!
//! Each executor exposes a `capabilities()` snapshot the harness main
//! loop pairs with `enforce_safety_policy()` before flipping a task to
//! Running. These tests cover both halves: the per-executor capability
//! shape under representative env configurations, and the policy
//! decision against tasks that carry an atom-derived `safety` profile.
//!
//! Lives in `tests/` (not in `src/executor/mod.rs::safety_tests`) so it
//! exercises the public library surface the way downstream callers
//! will — the in-crate tests already pin the decision logic against
//! synthetic `ExecutorCapabilities`; this target pins the executors'
//! own capability functions + their interaction with the gate.

// S5.32: workspace lint is `unsafe_code = "deny"`. This integration
// file uses `unsafe { std::env::set_var / remove_var }` to control
// ECAA_LOCAL_SANDBOX / ECAA_SLURM_NATIVE_CONTAINER vars (unsafe in
// Rust 2024 edition because the env table is not thread-safe). All
// call sites grab the shared ENV_LOCK below; the bounded waiver is
// scoped to this integration test target.
#![allow(unsafe_code)]

use ecaa_workflow_core::atom::{
    CodeExecution, NetworkPolicy, ProvisioningPolicy, SafetyLevel, SafetyPolicy, SandboxRequirement,
};
use ecaa_workflow_core::blocker::BlockerKind;
use ecaa_workflow_core::dag::{Assignee, ResourceClass, Task, TaskKind, TaskState};
use ecaa_workflow_harness::executor::{
    enforce_safety_policy, local::LocalExecutor, Executor, ExecutorArgs,
};
use std::sync::Mutex;

/// Shared mutex so tests mutating ECAA_LOCAL_SANDBOX /
/// ECAA_SLURM_NATIVE_CONTAINER env vars run one-at-a-time. `cargo
/// test` schedules tests across multiple threads by default; each
/// env-mutating test grabs this lock for its duration.
static ENV_LOCK: Mutex<()> = Mutex::new(());

fn args() -> ExecutorArgs {
    ExecutorArgs {
        package: "/tmp/harness-executor-safety".into(),
        agent: "/bin/true".into(),
        task_timeout_secs: 300,
    }
}

/// Build a `Task` carrying the requested `safety` and a stable
/// `source_atom_id` so the gate's diagnostic surfaces a deterministic
/// atom id. Mirrors the struct literal pattern from
/// `crates/core/src/dag.rs::pending_task`.
fn task_with_safety(safety: SafetyPolicy, source_atom_id: &str) -> Task {
    Task {
        kind: TaskKind::Computation,
        state: TaskState::Ready,
        depends_on: vec![],
        assignee: Assignee::Agent,
        description: "safety policy probe".into(),
        spec: None,
        resolution: None,
        result_ref: None,
        resource_class: ResourceClass::CpuHeavy,
        requires_sme_review: false,
        required_artifacts: vec![],
        container: None,
        source_atom_id: Some(source_atom_id.into()),
        safety,
    }
}

/// Safety profile for an Exec-level atom (generated-code path) that
/// REQUIRES ProcessIsolation. Matches the typical exec atom shape from
/// `config/stage-atoms/`.
fn exec_atom_safety() -> SafetyPolicy {
    SafetyPolicy {
        level: SafetyLevel::Exec,
        network: NetworkPolicy::None { allowlist: vec![] },
        sandbox: SandboxRequirement::ProcessIsolation,
        code_execution: CodeExecution::GeneratedByAgent,
        provisioning: ProvisioningPolicy::Allowlisted,
        controlled_access: false,
    }
}

/// Safety profile for a Network-level atom requesting `Bridge`
/// (full egress). Matches the literature-fetch / pubmed-style atoms.
fn network_bridge_atom_safety() -> SafetyPolicy {
    SafetyPolicy {
        level: SafetyLevel::Network,
        network: NetworkPolicy::Bridge,
        sandbox: SandboxRequirement::None,
        code_execution: CodeExecution::None,
        provisioning: ProvisioningPolicy::DeclaredOnly,
        controlled_access: false,
    }
}

/// Safety profile for a Compute-level atom — runs without egress
/// requirements and without process-isolation expectations. Passes the
/// gate on every executor.
fn compute_atom_safety() -> SafetyPolicy {
    SafetyPolicy {
        level: SafetyLevel::Compute,
        network: NetworkPolicy::None { allowlist: vec![] },
        sandbox: SandboxRequirement::None,
        code_execution: CodeExecution::None,
        provisioning: ProvisioningPolicy::DeclaredOnly,
        controlled_access: false,
    }
}

// ── LocalExecutor capabilities ─────────────────────────────────────

#[test]
fn local_capabilities_default_no_sandbox_bridge_network() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    // Pin ECAA_LOCAL_SANDBOX=off so detect_default_sandbox()'s bwrap probe
    // can't auto-detect a host-installed bwrap and override the assertion
    // — the test asserts the explicit opt-out default, not host
    // introspection.
    unsafe { std::env::set_var("ECAA_LOCAL_SANDBOX", "off") };
    let exec = LocalExecutor::new(&args());
    let caps = exec.capabilities();
    unsafe { std::env::remove_var("ECAA_LOCAL_SANDBOX") };
    assert_eq!(
        caps.sandbox,
        SandboxRequirement::None,
        "default local sandbox is None until bubblewrap is opted in"
    );
    assert!(
        matches!(caps.network, NetworkPolicy::Bridge),
        "local executor inherits host egress (Bridge)"
    );
    assert_eq!(caps.kind, "local");
}

#[test]
fn local_capabilities_bubblewrap_upgrades_to_process_isolation() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    unsafe { std::env::set_var("ECAA_LOCAL_SANDBOX", "bubblewrap") };
    let exec = LocalExecutor::new(&args());
    let caps = exec.capabilities();
    assert_eq!(
        caps.sandbox,
        SandboxRequirement::ProcessIsolation,
        "bubblewrap mode advertises ProcessIsolation to the gate"
    );
    unsafe { std::env::remove_var("ECAA_LOCAL_SANDBOX") };
}

#[test]
fn local_capabilities_unknown_sandbox_value_stays_none() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    unsafe { std::env::set_var("ECAA_LOCAL_SANDBOX", "garbage_value") };
    let exec = LocalExecutor::new(&args());
    let caps = exec.capabilities();
    assert_eq!(
        caps.sandbox,
        SandboxRequirement::None,
        "an unknown ECAA_LOCAL_SANDBOX value must fall back to None"
    );
    unsafe { std::env::remove_var("ECAA_LOCAL_SANDBOX") };
}

// ── LocalExecutor × enforce_safety_policy ─────────────────────────

#[test]
fn local_default_blocks_exec_atom_with_sandbox_required() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    // Pin ECAA_LOCAL_SANDBOX=off so detect_default_sandbox()'s bwrap probe
    // can't auto-detect a host-installed bwrap and silently satisfy
    // ProcessIsolation — the test asserts the sandbox-unavailable →
    // SandboxRequired path, not host introspection.
    unsafe { std::env::set_var("ECAA_LOCAL_SANDBOX", "off") };
    let exec = LocalExecutor::new(&args());
    let caps = exec.capabilities();
    unsafe { std::env::remove_var("ECAA_LOCAL_SANDBOX") };
    let task = task_with_safety(exec_atom_safety(), "generated_code_atom");
    let blocker = enforce_safety_policy(&task, &caps);
    match blocker {
        Some(BlockerKind::SandboxRequired {
            atom_id,
            requested,
            available,
        }) => {
            assert_eq!(atom_id, "generated_code_atom");
            assert_eq!(requested, SandboxRequirement::ProcessIsolation);
            assert_eq!(available, SandboxRequirement::None);
        }
        other => panic!(
            "expected SandboxRequired for Exec atom on default local, got {:?}",
            other
        ),
    }
}

#[test]
fn local_with_bubblewrap_passes_exec_atom() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    unsafe { std::env::set_var("ECAA_LOCAL_SANDBOX", "bubblewrap") };
    let exec = LocalExecutor::new(&args());
    let caps = exec.capabilities();
    let task = task_with_safety(exec_atom_safety(), "generated_code_atom");
    assert!(
        enforce_safety_policy(&task, &caps).is_none(),
        "bubblewrap-armed local must satisfy Exec atom's ProcessIsolation"
    );
    unsafe { std::env::remove_var("ECAA_LOCAL_SANDBOX") };
}

#[test]
fn local_passes_network_bridge_atom() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    unsafe { std::env::remove_var("ECAA_LOCAL_SANDBOX") };
    let exec = LocalExecutor::new(&args());
    let caps = exec.capabilities();
    let task = task_with_safety(network_bridge_atom_safety(), "pubmed_fetch_atom");
    assert!(
        enforce_safety_policy(&task, &caps).is_none(),
        "local advertises Bridge egress; a Bridge-requesting atom should pass"
    );
}

#[test]
fn local_passes_compute_atom_anywhere() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    unsafe { std::env::remove_var("ECAA_LOCAL_SANDBOX") };
    let exec = LocalExecutor::new(&args());
    let caps = exec.capabilities();
    let task = task_with_safety(compute_atom_safety(), "deseq2_atom");
    assert!(
        enforce_safety_policy(&task, &caps).is_none(),
        "Compute atoms pass the gate regardless of executor profile"
    );
}

// ── SlurmExecutor capabilities (only under `slurm` feature) ──────

#[cfg(feature = "slurm")]
mod slurm_safety {
    use super::*;
    use ecaa_workflow_harness::executor::slurm::sizing::{
        ResourceClass as SlurmResourceClass, SlurmMapping,
    };
    use ecaa_workflow_harness::executor::slurm::ssh::FakeSshSession;
    use ecaa_workflow_harness::executor::slurm::{SlurmConfig, SlurmExecutor};
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    fn stub_config() -> SlurmConfig {
        SlurmConfig {
            host: "test-host".into(),
            user: None,
            ssh_key: None,
            proxy_jump: None,
            staging_dir: PathBuf::from("/tmp/scripps-slurm-test"),
            default_partition: "compute".into(),
            account: None,
            default_qos: None,
            modules: vec![],
            poll_interval: std::time::Duration::from_secs(20),
            max_queue_wait: std::time::Duration::from_secs(60),
            default_time_limit: "01:00:00".into(),
        }
    }

    fn stub_mapping() -> SlurmMapping {
        SlurmMapping {
            version: 1,
            resource_classes: BTreeMap::new(),
            fallback: SlurmResourceClass {
                partition: "compute".into(),
                qos: None,
                cpus_per_task: 1,
                mem: "1G".into(),
                gres: None,
                time: "01:00:00".into(),
            },
            partitions: BTreeMap::new(),
        }
    }

    fn slurm_for_test() -> SlurmExecutor {
        // FakeSshSession is the production test transport — capabilities()
        // never invokes the SSH layer, but the trait object still needs a
        // valid impl so SlurmExecutor::with_ssh can take ownership.
        SlurmExecutor::with_ssh(
            args(),
            stub_config(),
            stub_mapping(),
            Box::new(FakeSshSession::new("test-host")),
        )
    }

    #[test]
    fn slurm_capabilities_default_no_sandbox_deny_egress() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        unsafe { std::env::remove_var("ECAA_SLURM_NATIVE_CONTAINER") };
        let exec = slurm_for_test();
        let caps = exec.capabilities();
        assert_eq!(
            caps.sandbox,
            SandboxRequirement::None,
            "default SLURM has no sandbox until native --container is opted in"
        );
        match &caps.network {
            NetworkPolicy::None { allowlist } => {
                assert!(allowlist.is_empty(), "SLURM defaults to deny-all egress");
            }
            other => panic!("expected None allowlist, got {:?}", other),
        }
        assert_eq!(caps.kind, "slurm");
    }

    #[test]
    fn slurm_capabilities_native_container_upgrades_sandbox() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        unsafe { std::env::set_var("ECAA_SLURM_NATIVE_CONTAINER", "1") };
        let exec = slurm_for_test();
        let caps = exec.capabilities();
        assert_eq!(
            caps.sandbox,
            SandboxRequirement::ProcessIsolation,
            "ECAA_SLURM_NATIVE_CONTAINER=1 advertises ProcessIsolation"
        );
        unsafe { std::env::remove_var("ECAA_SLURM_NATIVE_CONTAINER") };
    }

    #[test]
    fn slurm_default_blocks_bridge_network_atom() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        unsafe { std::env::remove_var("ECAA_SLURM_NATIVE_CONTAINER") };
        let exec = slurm_for_test();
        let caps = exec.capabilities();
        let task = task_with_safety(network_bridge_atom_safety(), "pubmed_fetch_atom");
        let blocker = enforce_safety_policy(&task, &caps);
        match blocker {
            Some(BlockerKind::NetworkPolicyMismatch { atom_id, .. }) => {
                assert_eq!(atom_id, "pubmed_fetch_atom");
            }
            other => panic!(
                "expected NetworkPolicyMismatch on SLURM deny-all, got {:?}",
                other
            ),
        }
    }

    #[test]
    fn slurm_default_blocks_exec_atom_with_sandbox_required() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        unsafe { std::env::remove_var("ECAA_SLURM_NATIVE_CONTAINER") };
        let exec = slurm_for_test();
        let caps = exec.capabilities();
        let task = task_with_safety(exec_atom_safety(), "generated_code_atom");
        let blocker = enforce_safety_policy(&task, &caps);
        assert!(
            matches!(blocker, Some(BlockerKind::SandboxRequired { .. })),
            "expected SandboxRequired on default SLURM, got {:?}",
            blocker
        );
    }

    #[test]
    fn slurm_native_container_passes_exec_atom() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        unsafe { std::env::set_var("ECAA_SLURM_NATIVE_CONTAINER", "1") };
        // SLURM's network stays deny-all even with native containers,
        // so we use a Compute-level safety profile (the bare Exec
        // safety wouldn't pass because the atom's network is
        // None{allowlist:[]} but the gate's network check is gated on
        // SafetyLevel::Exec — the bare Exec profile here has empty
        // allowlist which is a subset of executor's empty allowlist
        // so it passes too).
        let exec = slurm_for_test();
        let caps = exec.capabilities();
        let task = task_with_safety(exec_atom_safety(), "generated_code_atom");
        assert!(
            enforce_safety_policy(&task, &caps).is_none(),
            "Exec atom with empty network allowlist passes on native-container SLURM"
        );
        unsafe { std::env::remove_var("ECAA_SLURM_NATIVE_CONTAINER") };
    }

    #[test]
    fn slurm_passes_compute_atom_unconditionally() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        unsafe { std::env::remove_var("ECAA_SLURM_NATIVE_CONTAINER") };
        let exec = slurm_for_test();
        let caps = exec.capabilities();
        let task = task_with_safety(compute_atom_safety(), "deseq2_atom");
        assert!(
            enforce_safety_policy(&task, &caps).is_none(),
            "Compute atoms pass on SLURM (no sandbox / network gate at this level)"
        );
    }
}

// ── AwsExecutor capabilities ──────────────────────────────────────
//
// AwsExecutor::new is gated on ECAA_AWS_* env vars and constructs a
// duct-based command pipeline. Rather than fully stub the AWS CLI
// shell-out path here (covered by tests/executor.rs), we exercise the
// capability shape by constructing an executor through the env-gated
// constructor and reading its capabilities() output.

mod aws_safety {
    use super::*;
    use ecaa_workflow_harness::executor::aws::AwsExecutor;

    /// Populate every required ECAA_AWS_* env var so AwsExecutor::new
    /// succeeds. The values are placeholders — capabilities() doesn't
    /// shell out, so the AWS CLI never sees them.
    fn install_aws_env() {
        unsafe {
            std::env::set_var("ECAA_AWS_REGION", "us-east-1");
            std::env::set_var("ECAA_AWS_AMI_ID", "ami-test");
            std::env::set_var("ECAA_AWS_SECURITY_GROUP", "sg-test");
            std::env::set_var("ECAA_AWS_INSTANCE_PROFILE", "ip-test");
            std::env::set_var("ECAA_AWS_SUBNET_IDS", "subnet-a,subnet-b");
        }
    }

    fn clear_aws_env() {
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
    }

    #[test]
    fn aws_capabilities_default_process_isolation_bridge() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        install_aws_env();
        let exec = AwsExecutor::new(&args()).expect("aws constructor must succeed");
        let caps = exec.capabilities();
        assert_eq!(
            caps.sandbox,
            SandboxRequirement::ProcessIsolation,
            "AWS always runs the agent inside a container — ProcessIsolation floor"
        );
        assert!(
            matches!(caps.network, NetworkPolicy::Bridge),
            "AWS EC2 instances route through the IGW — Bridge equivalent"
        );
        assert_eq!(caps.kind, "aws");
        clear_aws_env();
    }

    #[test]
    fn aws_passes_exec_atom_requesting_process_isolation() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        install_aws_env();
        let exec = AwsExecutor::new(&args()).expect("aws constructor must succeed");
        let caps = exec.capabilities();
        let task = task_with_safety(exec_atom_safety(), "generated_code_atom");
        assert!(
            enforce_safety_policy(&task, &caps).is_none(),
            "AWS satisfies the Exec atom's ProcessIsolation requirement out-of-box"
        );
        clear_aws_env();
    }

    #[test]
    fn aws_passes_network_bridge_atom() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        install_aws_env();
        let exec = AwsExecutor::new(&args()).expect("aws constructor must succeed");
        let caps = exec.capabilities();
        let task = task_with_safety(network_bridge_atom_safety(), "pubmed_fetch_atom");
        assert!(
            enforce_safety_policy(&task, &caps).is_none(),
            "AWS's Bridge egress satisfies a Bridge-requesting Network atom"
        );
        clear_aws_env();
    }

    #[test]
    fn aws_blocks_atom_requiring_hardware_enclave() {
        // No AWS executor today advertises HardwareEnclave — when an
        // atom declares the strongest sandbox tier, even AWS must
        // refuse. Pin this so a future executor upgrade is forced to
        // surface the change explicitly.
        let _lock = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        install_aws_env();
        let exec = AwsExecutor::new(&args()).expect("aws constructor must succeed");
        let caps = exec.capabilities();
        let safety = SafetyPolicy {
            level: SafetyLevel::Exec,
            network: NetworkPolicy::None { allowlist: vec![] },
            sandbox: SandboxRequirement::HardwareEnclave,
            code_execution: CodeExecution::GeneratedByAgent,
            provisioning: ProvisioningPolicy::Allowlisted,
            controlled_access: false,
        };
        let task = task_with_safety(safety, "enclave_atom");
        match enforce_safety_policy(&task, &caps) {
            Some(BlockerKind::SandboxRequired {
                requested,
                available,
                ..
            }) => {
                assert_eq!(requested, SandboxRequirement::HardwareEnclave);
                assert_eq!(available, SandboxRequirement::ProcessIsolation);
            }
            other => panic!(
                "expected SandboxRequired for HardwareEnclave atom on AWS, got {:?}",
                other
            ),
        }
        clear_aws_env();
    }
}

// ── Marker round-trip ─────────────────────────────────────────────

#[test]
fn safety_policy_marker_roundtrips_through_blocker_parser() {
    // When a dispatch-time refusal lands, the harness
    // writes `format_safety_policy_marker(blocker)` into
    // `BlockedRecord.reason`. The server's promotion path
    // (`parse_agent_blocker_kind`) reads that line and reconstructs
    // the typed variant. Test the round-trip directly so a regression
    // in either side surfaces immediately.
    use ecaa_workflow_core::blocker::{format_safety_policy_marker, parse_agent_blocker_kind};

    let original = BlockerKind::SandboxRequired {
        atom_id: "test_atom".into(),
        requested: SandboxRequirement::ProcessIsolation,
        available: SandboxRequirement::None,
    };
    let marker = format_safety_policy_marker(&original).expect("marker must encode");
    let back = parse_agent_blocker_kind("", "test_atom", &marker, None);
    assert_eq!(back, original);

    let original = BlockerKind::NetworkPolicyMismatch {
        atom_id: "network_atom".into(),
        atom_network: NetworkPolicy::Bridge,
        executor_network: NetworkPolicy::None { allowlist: vec![] },
    };
    let marker = format_safety_policy_marker(&original).expect("marker must encode");
    let back = parse_agent_blocker_kind("", "network_atom", &marker, None);
    assert_eq!(back, original);

    let original = BlockerKind::ProvisioningDenied {
        atom_id: "compute_atom".into(),
        package: "samtools".into(),
        registry: "apt".into(),
    };
    let marker = format_safety_policy_marker(&original).expect("marker must encode");
    let back = parse_agent_blocker_kind("", "compute_atom", &marker, None);
    assert_eq!(back, original);
}
