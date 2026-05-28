//! Phase C7 — integration tests for `BubblewrapRunner`.
//!
//! All tests operate over the `render_args` output (pure-fn over policy
//! structs) and therefore do not require `bwrap` to be installed.
//! Tests that actually spawn a bwrap process are marked `#[ignore]` and
//! must be opted in explicitly with `cargo test -- --ignored`.
//!
//! Run the pure tests:
//! cargo test -p ecaa-workflow-harness sandbox_runner
//!
//! Run the spawn tests (requires bwrap on PATH):
//! cargo test -p ecaa-workflow-harness sandbox_runner -- --ignored

// S5.32 — `unsafe` waiver scoped to env-mutation in test setup/teardown.
#![allow(unsafe_code)]

use ecaa_workflow_core::sandbox_policy::SandboxPolicy;
use ecaa_workflow_harness::sandbox_enforcer::{BubblewrapRunner, SandboxRunnerError};
use std::path::PathBuf;

/// Helper: a strict policy with a specific field overridden.
fn policy_with(f: impl FnOnce(&mut SandboxPolicy)) -> SandboxPolicy {
    let mut p = SandboxPolicy::default_strict();
    f(&mut p);
    p
}

/// Lock for tests that mutate `SWFC_LOCAL_SANDBOX` in the process env.
/// Without serialisation, parallel `cargo test` runs observe each
/// other's transient overrides.
static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

// ── Pure render_args tests ────────────────────────────────────────────────

#[test]
fn wraps_with_unshare_net_when_deny_network_true() {
    let runner = BubblewrapRunner::new_for_test(PathBuf::from("/pkg"));
    let policy = policy_with(|p| p.deny_network = true);
    let args = runner.render_args(&policy);
    assert!(
        args.iter().any(|a| a == "--unshare-net"),
        "expected --unshare-net in: {:?}",
        args
    );
}

#[test]
fn no_unshare_net_when_deny_network_false() {
    let runner = BubblewrapRunner::new_for_test(PathBuf::from("/pkg"));
    let policy = policy_with(|p| p.deny_network = false);
    let args = runner.render_args(&policy);
    assert!(
        !args.iter().any(|a| a == "--unshare-net"),
        "must NOT have --unshare-net when deny_network=false, got: {:?}",
        args
    );
}

#[test]
fn unsets_secret_env_vars_when_deny_secrets_true() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    // Plant a recognisable secret var.
    let secret_key = "TEST_SWFC_C7_MY_API_KEY";
    unsafe { std::env::set_var(secret_key, "super-secret") };

    let runner = BubblewrapRunner::new_for_test(PathBuf::from("/pkg"));
    let policy = policy_with(|p| p.deny_secrets = true);
    let args = runner.render_args(&policy);

    // Cleanup before assertions so a panic doesn't leak the var.
    unsafe { std::env::remove_var(secret_key) };

    // The rendered args must contain `--unsetenv TEST_SWFC_C7_MY_API_KEY`.
    let pairs: Vec<(&str, &str)> = args
        .windows(2)
        .filter_map(|w| {
            if w[0] == "--unsetenv" {
                Some((w[0].as_str(), w[1].as_str()))
            } else {
                None
            }
        })
        .collect();
    let unset_keys: Vec<&str> = pairs.iter().map(|(_, k)| *k).collect();
    assert!(
        unset_keys.contains(&secret_key),
        "expected {secret_key} in --unsetenv list, got: {:?}",
        unset_keys
    );
}

#[test]
fn deny_secrets_false_does_not_emit_unsetenv() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let runner = BubblewrapRunner::new_for_test(PathBuf::from("/pkg"));
    // Clear allow_envs as well so we're testing deny_secrets in isolation —
    // allow_envs is an independent axis that also emits --unsetenv when
    // non-empty.
    let policy = policy_with(|p| {
        p.deny_secrets = false;
        p.allow_envs = Vec::new();
    });
    let args = runner.render_args(&policy);
    assert!(
        !args.iter().any(|a| a == "--unsetenv"),
        "must NOT emit --unsetenv when deny_secrets=false and allow_envs is empty"
    );
}

#[test]
fn passthrough_when_mode_off() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    unsafe { std::env::set_var("SWFC_LOCAL_SANDBOX", "off") };
    let result = BubblewrapRunner::from_env(PathBuf::from("/pkg"));
    unsafe { std::env::remove_var("SWFC_LOCAL_SANDBOX") };
    assert!(
        matches!(result, Ok(None)),
        "off mode must return Ok(None), got: {:?}",
        result
    );
}

#[test]
fn passthrough_when_mode_unset() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    unsafe { std::env::remove_var("SWFC_LOCAL_SANDBOX") };
    let result = BubblewrapRunner::from_env(PathBuf::from("/pkg"));
    assert!(
        matches!(result, Ok(None)),
        "unset SWFC_LOCAL_SANDBOX must return Ok(None), got: {:?}",
        result
    );
}

#[test]
fn errors_when_bwrap_missing_and_mode_bubblewrap() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    unsafe { std::env::set_var("SWFC_LOCAL_SANDBOX", "bubblewrap") };
    // Override the bwrap path to a nonexistent location via the
    // test-only constructor. `from_env` hardcodes /usr/bin/bwrap so we
    // test the error path by constructing with a bogus path.
    let result = BubblewrapRunner::from_env_with_bwrap_path(
        PathBuf::from("/nonexistent/bwrap"),
        PathBuf::from("/pkg"),
    );
    unsafe { std::env::remove_var("SWFC_LOCAL_SANDBOX") };
    assert!(
        matches!(result, Err(SandboxRunnerError::BwrapBinaryMissing(_))),
        "missing bwrap must produce BwrapBinaryMissing error, got: {:?}",
        result
    );
}

#[test]
fn args_are_deterministic_per_policy() {
    let runner = BubblewrapRunner::new_for_test(PathBuf::from("/pkg"));
    let policy = SandboxPolicy::default_strict();
    let first = runner.render_args(&policy);
    let second = runner.render_args(&policy);
    assert_eq!(
        first, second,
        "render_args must be deterministic (same policy → same args)"
    );
}

#[test]
fn wraps_with_prlimit_when_memory_limit_set() {
    let runner = BubblewrapRunner::new_for_test(PathBuf::from("/pkg"));
    let policy = policy_with(|p| {
        p.memory_limit_mb = Some(4096);
        p.wall_timeout_secs = None;
    });
    let program = "/bin/echo";
    let cmd = runner.wrap(program, &[], &policy);
    // The first argument to Command is the argv0. When memory is set
    // and timeout is not, argv0 should be prlimit.
    let program_str = format!("{:?}", cmd.get_program());
    assert!(
        program_str.contains("prlimit"),
        "expected prlimit as argv0 when memory_limit_mb is set, got: {}",
        program_str
    );
}

#[test]
fn wraps_with_timeout_when_wall_timeout_set() {
    let runner = BubblewrapRunner::new_for_test(PathBuf::from("/pkg"));
    let policy = policy_with(|p| {
        p.memory_limit_mb = None;
        p.wall_timeout_secs = Some(300);
    });
    let cmd = runner.wrap("/bin/echo", &[], &policy);
    let program_str = format!("{:?}", cmd.get_program());
    assert!(
        program_str.contains("timeout"),
        "expected timeout as argv0 when wall_timeout_secs is set, got: {}",
        program_str
    );
}

#[test]
fn wraps_with_timeout_then_prlimit_when_both_set() {
    let runner = BubblewrapRunner::new_for_test(PathBuf::from("/pkg"));
    let policy = SandboxPolicy::default_strict(); // has both limits
    let cmd = runner.wrap("/bin/echo", &[], &policy);
    // argv0 must be `timeout` (outermost wrapper).
    let program_str = format!("{:?}", cmd.get_program());
    assert!(
        program_str.contains("timeout"),
        "expected timeout as outermost wrapper when both limits are set, got: {}",
        program_str
    );
    // prlimit must appear somewhere in the arg list.
    let args: Vec<String> = cmd
        .get_args()
        .map(|a| a.to_string_lossy().to_string())
        .collect();
    assert!(
        args.iter().any(|a| a.contains("prlimit")),
        "expected prlimit in args when both limits are set, got: {:?}",
        args
    );
}

#[test]
fn deny_with_parent_and_new_session_always_present() {
    let runner = BubblewrapRunner::new_for_test(PathBuf::from("/pkg"));
    // Verify with a very permissive policy (all deny flags false) so we
    // confirm the safety flags are unconditional.
    let policy = policy_with(|p| {
        p.deny_network = false;
        p.deny_secrets = false;
        p.deny_host_fs = false;
        p.memory_limit_mb = None;
        p.wall_timeout_secs = None;
    });
    let args = runner.render_args(&policy);
    assert!(
        args.contains(&"--die-with-parent".to_string()),
        "--die-with-parent must always be present"
    );
    assert!(
        args.contains(&"--new-session".to_string()),
        "--new-session must always be present"
    );
}

#[test]
fn deny_host_fs_adds_ro_bind_for_workdir() {
    let workdir = PathBuf::from("/tmp/test_workdir");
    let runner = BubblewrapRunner::new_for_test(workdir.clone());
    let policy = policy_with(|p| p.deny_host_fs = true);
    let args = runner.render_args(&policy);
    // Workdir must appear as a --bind (RW) mount.
    let bind_positions: Vec<usize> = args
        .iter()
        .enumerate()
        .filter_map(|(i, a)| if a == "--bind" { Some(i) } else { None })
        .collect();
    let workdir_bound = bind_positions
        .iter()
        .any(|&i| args.get(i + 1).map(|s| s.as_str()) == Some(workdir.to_str().unwrap()));
    assert!(
        workdir_bound,
        "workdir {:?} must be --bind mounted when deny_host_fs=true, args: {:?}",
        workdir, args
    );
}

#[test]
fn policy_digest_is_stable() {
    let policy = SandboxPolicy::default_strict();
    let d1 = BubblewrapRunner::policy_digest(&policy);
    let d2 = BubblewrapRunner::policy_digest(&policy);
    assert_eq!(d1, d2, "policy_digest must be stable across calls");
    assert_eq!(d1.len(), 16, "digest must be 16 hex chars");
}

#[test]
fn policy_digest_differs_for_different_policies() {
    let strict = SandboxPolicy::default_strict();
    let permissive = policy_with(|p| p.deny_network = false);
    let d_strict = BubblewrapRunner::policy_digest(&strict);
    let d_permissive = BubblewrapRunner::policy_digest(&permissive);
    assert_ne!(
        d_strict, d_permissive,
        "different policies must produce different digests"
    );
}

/// Verify that when `allow_envs` is non-empty, env vars NOT in the
/// allowlist produce `--unsetenv <name>` args, and vars in the allowlist
/// do NOT produce `--unsetenv` for their names.
#[test]
fn args_allow_envs_unsets_others() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());

    // Plant two recognisable non-secret test vars.
    let foo_key = "TEST_SWFC_C14_FOO";
    let baz_key = "TEST_SWFC_C14_BAZ";
    unsafe { std::env::set_var(foo_key, "bar") };
    unsafe { std::env::set_var(baz_key, "qux") };

    let runner = BubblewrapRunner::new_for_test(PathBuf::from("/pkg"));
    let policy = policy_with(|p| {
        // Use PATH as the only allowlist entry; deny_secrets=false so
        // the secret-pattern path doesn't confound the assertion.
        p.deny_secrets = false;
        p.allow_envs = vec!["PATH".into()];
    });
    let args = runner.render_args(&policy);

    // Cleanup before assertions so a panic doesn't leak the vars.
    unsafe { std::env::remove_var(foo_key) };
    unsafe { std::env::remove_var(baz_key) };

    // Collect all --unsetenv targets.
    let unset_keys: Vec<&str> = args
        .windows(2)
        .filter_map(|w| {
            if w[0] == "--unsetenv" {
                Some(w[1].as_str())
            } else {
                None
            }
        })
        .collect();

    // FOO and BAZ must be in the --unsetenv list (not in allowlist).
    assert!(
        unset_keys.contains(&foo_key),
        "expected {foo_key} in --unsetenv list, got: {:?}",
        unset_keys
    );
    assert!(
        unset_keys.contains(&baz_key),
        "expected {baz_key} in --unsetenv list, got: {:?}",
        unset_keys
    );

    // PATH must NOT be in the --unsetenv list (it IS in the allowlist).
    assert!(
        !unset_keys.contains(&"PATH"),
        "PATH must NOT be --unsetenv'd when it's in the allowlist, got: {:?}",
        unset_keys
    );
}

// ── Spawn tests (require bwrap at /usr/bin/bwrap) ────────────────────────

/// Verify that bwrap actually drops network access when --unshare-net is set.
/// Guards on bwrap being present at /usr/bin/bwrap; skips cleanly when absent
/// so CI hosts without bwrap don't fail.
#[test]
fn bwrap_spawn_unshare_net_prevents_loopback() {
    if !std::path::Path::new("/usr/bin/bwrap").exists() {
        eprintln!("[skip] bwrap binary missing at /usr/bin/bwrap");
        return;
    }

    let _lock = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let workdir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/tmp"));
    unsafe { std::env::set_var("SWFC_LOCAL_SANDBOX", "bubblewrap") };
    let runner = BubblewrapRunner::from_env(workdir.clone())
        .expect("from_env ok")
        .expect("runner should be Some in bubblewrap mode");
    unsafe { std::env::remove_var("SWFC_LOCAL_SANDBOX") };

    let policy = policy_with(|p| {
        p.deny_network = true;
        p.deny_host_fs = false;
        p.deny_secrets = false;
        p.memory_limit_mb = None;
        p.wall_timeout_secs = None;
    });
    // ping 127.0.0.1 should fail because the network namespace is isolated.
    let status = runner
        .wrap(
            "/usr/bin/ping",
            &["-c", "1", "-W", "1", "127.0.0.1"],
            &policy,
        )
        .status()
        .expect("spawn failed");
    assert!(
        !status.success(),
        "ping to loopback should fail inside a --unshare-net sandbox"
    );
}
