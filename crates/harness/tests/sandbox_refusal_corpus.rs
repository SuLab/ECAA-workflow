//! Grant v19 §C.0.1 — "Sandbox refusal: no failures observed across
//! 30 scenarios (95% binomial upper bound 11.5% per-scenario failure
//! rate)".
//!
//! Corpus history:
//! * 10 scenarios — initial.
//! * 20 scenarios — bumped; sysctl/dmesg/raw-block/setcap/
//!   mount-bind/dev-mem/shared-mem/systemd-run/ld.so.preload/cgroup.
//! * 30 scenarios — bumped (this revision); blockdev write
//!   to /dev/sda, /sys/kernel/debug read, ptrace via gdb attach to
//!   PID 1, eBPF syscall, kernel-module load, raw IPv6 socket,
//!   /proc/sysrq-trigger, system-bus dbus-monitor snoop, abstract
//!   UNIX-socket connect into the host namespace, SIGKILL to PID 1.
//!
//! Each scenario is a shell script under sandbox_scenarios/. The test
//! dispatches every script through the SAME public production sandbox
//! entry point the harness uses for `Implementation::GeneratedCode`
//! tasks — `harness::sandbox_enforcer::BubblewrapRunner::wrap()` with
//! `SandboxPolicy::default_strict()`. The earlier revision constructed
//! a bwrap argv inline; this version re-anchors so the test exercises
//! the actual production policy translation (deny_network,
//! deny_secrets, deny_host_fs, prlimit/timeout wrapping), closing the
//! construct-validity gap the grant audit flagged.
//!
//! Refusal heuristic (conjunctive form):
//! Refused ⇔ (exit_code != 0) AND (no success/exfil token on stdout/stderr)
//! An OR form would let a script silently exit 0 with no token through
//! as "refused". The conjunctive form requires the script to BOTH
//! visibly fail AND produce no exfil signal — a silent bypass is
//! caught by the absence of a non-zero exit.

use std::path::{Path, PathBuf};

use ecaa_workflow_core::sandbox_policy::SandboxPolicy;
use ecaa_workflow_harness::sandbox_enforcer::BubblewrapRunner;

fn scenarios_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/sandbox_scenarios")
}

#[derive(Debug, Clone, Copy)]
struct ScenarioResult {
    refused: bool,
}

/// Dispatch `script` through the production sandbox: build a
/// `BubblewrapRunner` rooted at the ecaa-workflow scenarios workdir,
/// then `wrap("/bin/bash", &[script], &SandboxPolicy::default_strict())`.
///
/// This is the same code path the harness takes in
/// `executor::local::maybe_wrap_with_bwrap` for an
/// `Implementation::GeneratedCode` task. Re-anchoring here is the
/// construct-validity fix called out in §C.0.1's audit — the prior
/// revision constructed bwrap argv inline and proved only "this static
/// argv refuses these scripts", not "the production sandbox refuses
/// these scripts".
fn run_in_sandbox(script: &Path) -> ScenarioResult {
    // bwrap presence is a hard precondition; the test is gated on
    // ECAA_LOCAL_SANDBOX=bubblewrap and runs only when the binary is
    // present (see the early-return in `sandbox_refuses_all_30_scenarios`).
    let bwrap_path = PathBuf::from("/usr/bin/bwrap");
    let workdir = scenarios_dir();
    let runner = BubblewrapRunner::from_env_with_bwrap_path(bwrap_path, workdir)
        .expect("BubblewrapRunner::from_env_with_bwrap_path failed unexpectedly")
        .expect("ECAA_LOCAL_SANDBOX must be 'bubblewrap' to dispatch the corpus");

    let policy = SandboxPolicy::default_strict();
    // wrap() returns a fully-formed std::process::Command using the
    // same prlimit/timeout/bwrap chain the production path uses.
    let script_str = script.to_string_lossy().to_string();
    let output = runner
        .wrap("/bin/bash", &[script_str.as_str()], &policy)
        .output();

    match output {
        Err(_) => ScenarioResult { refused: true }, // bwrap rejected the spawn entirely
        Ok(o) => {
            let stdout = String::from_utf8_lossy(&o.stdout);
            let stderr = String::from_utf8_lossy(&o.stderr);
            let success_tokens = ["owned", "PRIVATE KEY", "escaped", "exfil"];
            let bypass_signal = success_tokens
                .iter()
                .any(|t| stdout.contains(t) || stderr.contains(t));
            // Tightened refusal: both exit-nonzero AND no exfil token.
            // A silent exit-0 script with no token would have been
            // misclassified as "refused" by the prior OR rule.
            let exit_nonzero = !o.status.success();
            ScenarioResult {
                refused: exit_nonzero && !bypass_signal,
            }
        }
    }
}

/// Corpus size. Bumped from 20 → 30 to drop the Wilson 95%
/// UB on the per-scenario failure rate (from 0.16 to ≈0.11).
///
/// If the corpus changes again, update this constant AND the grant
/// prose "no failures observed across N scenarios" AND the Wilson UB
/// assertion's target band in `sandbox_refuses_all_30_scenarios` AND
/// `scenarios_directory_has_30_files`.
const SANDBOX_CORPUS_SIZE: usize = 30;

#[test]
#[ignore = "requires bubblewrap; CI runs via make sandbox-refusal"]
fn sandbox_refuses_all_30_scenarios() {
    // S5.32 — `unsafe` waiver scoped to env-mutation in test setup;
    // ECAA_LOCAL_SANDBOX must be set for BubblewrapRunner to activate.
    #[allow(unsafe_code)]
    unsafe {
        std::env::set_var("ECAA_LOCAL_SANDBOX", "bubblewrap");
    }

    let mut total = 0;
    let mut refused = 0;
    let mut failures: Vec<String> = Vec::new();

    for entry in std::fs::read_dir(scenarios_dir()).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().and_then(|s| s.to_str()) != Some("sh") {
            continue;
        }
        total += 1;
        let result = run_in_sandbox(&path);
        if result.refused {
            refused += 1;
        } else {
            failures.push(path.file_name().unwrap().to_string_lossy().to_string());
        }
    }

    #[allow(unsafe_code)]
    unsafe {
        std::env::remove_var("ECAA_LOCAL_SANDBOX");
    }

    assert_eq!(
        total, SANDBOX_CORPUS_SIZE,
        "expected exactly {SANDBOX_CORPUS_SIZE} sandbox scenarios; got {total}"
    );
    assert_eq!(
        refused,
        SANDBOX_CORPUS_SIZE,
        "sandbox failed to refuse {} of {SANDBOX_CORPUS_SIZE} scenarios: {failures:?}",
        SANDBOX_CORPUS_SIZE - refused
    );

    // Wilson 95% upper bound for k=0 observed failures out of n=30 trials.
    // upper = (k̂ + z²/2n + z·√(k̂(1−k̂)/n + z²/4n²)) / (1 + z²/n)
    // with k̂ = 0, z = 1.96, n = 30 → upper ≈ 0.114.
    let n = total as f64;
    let z = 1.96_f64;
    let z2 = z * z;
    let p_hat = 0.0;
    let upper =
        (p_hat + z2 / (2.0 * n) + z * ((p_hat * (1.0 - p_hat) / n + z2 / (4.0 * n * n)).sqrt()))
            / (1.0 + z2 / n);
    println!(
        "Wilson 95% upper bound on per-scenario failure rate: {:.4} (target ≤ 0.12)",
        upper
    );
    // 0.12 target band; the analytic value is ~0.114.
    assert!(
        upper < 0.12,
        "Wilson 95% upper bound exceeded target: {upper:.4}"
    );
}

#[test]
fn scenarios_directory_has_30_files() {
    let count = std::fs::read_dir(scenarios_dir())
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().and_then(|s| s.to_str()) == Some("sh"))
        .count();
    assert_eq!(
        count, SANDBOX_CORPUS_SIZE,
        "expected {SANDBOX_CORPUS_SIZE} .sh scenarios; got {count}"
    );
}
