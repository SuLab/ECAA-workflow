//! Stall-monitor integration test for LocalExecutor. Drives a
//! `bash -c "sleep N"` subprocess through `LocalExecutor::run_iteration`
//! with the stall monitor armed and asserts a `CpuStarvation` signal
//! lands on the mpsc receiver. Linux-only — the monitor reads
//! `/proc/<pid>/stat` and `/proc/meminfo`, which are unavailable on
//! macOS and Windows.

use scripps_workflow_harness::executor::stall_monitor::{StallSignal, StallThresholds};
use scripps_workflow_harness::executor::{Executor, ExecutorArgs};
use std::os::unix::fs::PermissionsExt;
use std::sync::mpsc;
use std::time::{Duration, Instant};

#[cfg(target_os = "linux")]
#[test]
fn local_stall_monitor_fires_on_idle_subprocess() {
    // Regression config: `cpu_window_mins = 0` → window size clamps to
    // 1 sample, so the first observation below `cpu_min_pct` fires.
    // `cpu_min_pct = 101.0` is intentionally unreachable — guarantees
    // a sleeping subprocess (near-0% CPU) trips the starvation branch.
    let thresholds = StallThresholds {
        enabled: true,
        cpu_min_pct: 101.0,
        cpu_window_mins: 0,
        // Push memory threshold out of range for the test's host — the
        // test only cares about the CPU starvation branch, and we don't
        // want a memory-pressure false positive pre-empting it.
        mem_max_pct: 100.0,
        mem_window_mins: 5,
        gpu_idle_when_training_mins: 15,
        runtime_over_expected_mult: 2.0,
        sample_interval_secs: 1,
    };

    // Temp "agent" script that just sleeps — a dummy long-running,
    // CPU-idle subprocess. LocalExecutor::run_iteration invokes the
    // agent with the package dir as argv[1], so the script ignores its
    // argument. 10s sleep gives the monitor thread plenty of headroom
    // to see the subprocess, sample CPU, classify it, and send the
    // signal before the agent exits.
    let scratch = tempfile::tempdir().expect("tempdir");
    let pkg = scratch.path().join("pkg");
    std::fs::create_dir_all(&pkg).expect("pkg dir");
    let agent = scratch.path().join("idle-agent.sh");
    std::fs::write(&agent, "#!/usr/bin/env bash\nsleep 10\nexit 0\n").expect("write agent");
    let mut perms = std::fs::metadata(&agent).expect("metadata").permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&agent, perms).expect("chmod");

    let args = ExecutorArgs {
        package: pkg.to_string_lossy().to_string(),
        agent: agent.to_string_lossy().to_string(),
        task_timeout_secs: 300,
    };
    let mut exec = scripps_workflow_harness::executor::local::LocalExecutor::new(&args);

    let (tx, rx): (mpsc::Sender<StallSignal>, mpsc::Receiver<StallSignal>) = mpsc::channel();
    exec.start_stall_monitor(&thresholds, tx)
        .expect("start monitor");

    // run_iteration blocks until the agent exits, so drive it from a
    // worker thread and drain the receiver on the main thread.
    let pkg_for_thread = pkg.clone();
    let agent_for_thread = agent.to_string_lossy().to_string();
    let runner = std::thread::spawn(move || {
        let envelope = std::collections::BTreeMap::<String, String>::new();
        exec.run_iteration(&pkg_for_thread, &agent_for_thread, &envelope)
    });

    // Poll the receiver up to 8s — two or three samples at 1s cadence
    // is plenty for the clamped one-slot window.
    let deadline = Instant::now() + Duration::from_secs(8);
    let mut fired = None;
    while Instant::now() < deadline {
        match rx.recv_timeout(Duration::from_secs(1)) {
            Ok(signal) => {
                fired = Some(signal);
                break;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    // Drain the runner thread so the subprocess doesn't leak.
    let _ = runner.join();

    let signal = fired
        .expect("stall monitor should fire CpuStarvation within ~8s for an idle sleep subprocess");
    match signal {
        StallSignal::CpuStarvation { avg_cpu_pct, .. } => {
            assert!(
                avg_cpu_pct < 101.0,
                "sleeping subprocess should report near-0% CPU, got {avg_cpu_pct}"
            );
        }
        other => panic!(
            "expected CpuStarvation, got {:?}. The monitor may have picked up memory pressure unexpectedly.",
            other
        ),
    }
}

#[cfg(not(target_os = "linux"))]
#[test]
#[ignore = "stall monitor requires /proc, Linux-only"]
fn local_stall_monitor_fires_on_idle_subprocess() {
    eprintln!("local_stall_monitor_fires_on_idle_subprocess: skipped (requires /proc, Linux-only)");
}
