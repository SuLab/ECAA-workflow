//! Live-cluster SLURM integration test.
//!
//! "One `#[ignore]`'d test gated on
//! `ECAA_SLURM_HOST` + `ECAA_SLURM_STAGING_DIR`. Submits a trivial
//! `sleep 1` job, asserts terminal state + exit code. Documented in the
//! operator runbook."
//!
//! Run explicitly when SSH access to a real cluster is configured:
//!
//! ```sh
//! export ECAA_SLURM_HOST=login.cluster.example.org
//! export ECAA_SLURM_STAGING_DIR=/scratch/$USER/scripps
//! export ECAA_SLURM_DEFAULT_PARTITION=normal
//! cargo test -p ecaa-workflow-harness --features slurm \
//! --test slurm_live -- --ignored
//! ```
//!
//! Without those env vars the `#[ignore]` gate keeps this out of the
//! default workspace test run. The test is counted in
//! `rust_ignored_live_api_total` in `.github/ci/expected-test-counts.json`.
//!
//! F16-3: the entire file is gated behind the `slurm` cargo feature
//! (research-tier). Without the feature the file compiles to nothing,
//! mirroring the `executor::slurm` module's own gate.

#![cfg(feature = "slurm")]

use ecaa_workflow_harness::executor::slurm::polling::{query_job, JobState};
use ecaa_workflow_harness::executor::slurm::sbatch::{parse_job_id, submit_sbatch};
use ecaa_workflow_harness::executor::slurm::ssh::{SshSession, SystemSshSession};

/// Submits a trivial `sbatch` job, polls `sacct` to terminal state,
/// and asserts it reached `COMPLETED`. Cancels the job on timeout.
#[test]
#[ignore = "live-cluster test: requires ECAA_SLURM_HOST + ECAA_SLURM_STAGING_DIR + SSH access. Run manually with --ignored. See plan §8 + docs/remote-compute-operator-reference.md."]
fn live_slurm_sleep_job_reaches_terminal_state() {
    let host = std::env::var("ECAA_SLURM_HOST")
        .expect("ECAA_SLURM_HOST must be set for the live SLURM test");
    let staging = std::env::var("ECAA_SLURM_STAGING_DIR")
        .expect("ECAA_SLURM_STAGING_DIR must be set for the live SLURM test");
    let partition =
        std::env::var("ECAA_SLURM_DEFAULT_PARTITION").unwrap_or_else(|_| "normal".into());
    let user = std::env::var("ECAA_SLURM_USER").ok();
    let key = std::env::var("ECAA_SLURM_SSH_KEY")
        .ok()
        .map(std::path::PathBuf::from);
    let proxy = std::env::var("ECAA_SLURM_PROXY_JUMP").ok();

    let ssh = SystemSshSession::new(host.clone(), user, key, proxy)
        .expect("building SystemSshSession against live host");

    // Probe the cluster is reachable before we submit.
    let sinfo = ssh
        .run("sinfo -h -o '%P' | head -n1")
        .expect("sinfo probe failed (transport error)");
    assert!(
        sinfo.is_success(),
        "cluster not reachable: sinfo exit {} stderr: {}",
        sinfo.exit_code,
        sinfo.stderr
    );

    // Minimal sbatch script that runs `sleep 1` and exits 0. Staged to
    // a unique path under $STAGING_DIR so a stale run doesn't collide.
    let job_name = format!("scripps-live-{}", std::process::id());
    let script_path = format!("{}/{job_name}.sbatch", staging.trim_end_matches('/'));
    let script_body = format!(
        "#!/bin/bash\n#SBATCH --job-name={job_name}\n#SBATCH --partition={partition}\n#SBATCH --cpus-per-task=1\n#SBATCH --mem=128M\n#SBATCH --time=00:02:00\n#SBATCH --output={}/{job_name}.log\n\nsleep 1\n",
        staging.trim_end_matches('/')
    );

    let job_id = submit_sbatch(&ssh, &script_path, &script_body).expect("sbatch submission failed");
    assert!(parse_job_id(&job_id).is_some() || !job_id.is_empty());
    println!("[live-slurm] submitted job_id={job_id}");

    // Poll sacct up to 3 minutes (handles queue wait on busy clusters).
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(180);
    let row = loop {
        match query_job(&ssh, &job_id) {
            Ok(Some(row)) if row.state.is_terminal() => break row,
            Ok(_) => {}
            Err(e) => panic!("sacct error: {e}"),
        }
        if std::time::Instant::now() >= deadline {
            // Best-effort scancel to avoid leaving the job around.
            let _ = ssh.run(&format!("scancel {job_id}"));
            panic!("live SLURM job {job_id} did not reach terminal state within 3 minutes");
        }
        std::thread::sleep(std::time::Duration::from_secs(10));
    };

    println!("[live-slurm] terminal state: {:?}", row.state);
    assert_eq!(
        row.state,
        JobState::Completed,
        "live sleep 1 job must COMPLETE (got {:?}, exit={:?})",
        row.state,
        row.exit_code
    );
    assert_eq!(
        row.state.to_exit_code(row.exit_code),
        0,
        "COMPLETED sleep 1 must exit 0"
    );

    // Best-effort cleanup of the script + output files.
    let _ = ssh.run(&format!(
        "rm -f {script_path} {}/{job_name}.log",
        staging.trim_end_matches('/')
    ));
}
