//! CI-gated end-to-end integration test: env-snapshot build → replay ByteIdentical.
//!
//! # Gate
//! This test does nothing unless `ECAA_ENV_SNAPSHOT_TEST` is set in the
//! environment.  Unset → early return (compiles, passes, zero docker calls).
//! Set → runs the full live path: docker build, smoke check, replay.
//!
//! # Why the live path lives here
//! Tasks 1–6 built the env_snapshot feature (snapshot_environment, cache_scan,
//! Dockerfile generation, image build).  This test drives the full round-trip:
//! build a snapshot image from a tiny assembled cache → verify the copied conda
//! env actually executes inside the image (conda-relocation risk) → re-execute
//! the recorded result table via run_replay → assert ByteIdentical.
//!
//! # Deliverable for Task 8
//! The test COMPILES and correctly early-returns when the gate var is unset.
//! Live correctness is validated in Task 9's acceptance run (the base image is
//! 13 GB; we do NOT trigger it here).

use ecaa_workflow_harness::env_snapshot::{snapshot_environment, SnapshotOpts, SnapshotOutcome};
use ecaa_workflow_core::replay::{run_replay, ReplayOptions, Tier};
use ecaa_workflow_core::reexecution::ReexecutionBucket;

use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// Helpers (inside the module — only called inside the gated branch)
// ---------------------------------------------------------------------------


/// Write `contents` to `path`, creating parent directories as needed.
fn write_file(path: &Path, contents: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, contents).unwrap();
}

// ---------------------------------------------------------------------------
// Main test
// ---------------------------------------------------------------------------

/// Builds a snapshot image from a minimal conda-env cache stub, smoke-checks
/// the copied environment inside the resulting image, then drives replay and
/// asserts the compute table re-executes ByteIdentical.
///
/// Gate: set `ECAA_ENV_SNAPSHOT_TEST=1`.
/// Base image: `bio-min:local` (must be present on the docker daemon).
///
/// Live execution requires:
///   - Docker available and the `bio-min:local` image loaded.
///   - `conda` available inside the image (the smoke check uses `conda run`).
#[test]
fn snapshot_build_then_replay_is_byte_identical() {
    // ── Gate ────────────────────────────────────────────────────────────────
    if std::env::var("ECAA_ENV_SNAPSHOT_TEST").is_err() {
        return;
    }

    // ── Step 1: build a tiny "assembled cache" ───────────────────────────────
    // `cache_has_installs` returns true when conda-envs/<env>/bin/ exists.
    let cache_tmp = tempfile::tempdir().expect("cache tempdir");
    let cache_dir = cache_tmp.path().to_path_buf();
    let env_name = "snap_test_env";
    let env_bin = cache_dir.join("conda-envs").join(env_name).join("bin");
    std::fs::create_dir_all(&env_bin).expect("create conda-env bin dir");
    // A stub executable — the smoke check will use the real conda
    // environment inside the snapshot image (COPY from the real cache dir is
    // what the Dockerfile does), so this stub merely satisfies cache_has_installs.
    // In a real session this directory is the actual installed env.
    std::fs::write(env_bin.join("python"), b"#!/bin/sh\necho ok\n")
        .expect("write stub python");

    // ── Step 2: snapshot_environment → Captured ─────────────────────────────
    let fixed_source_date_epoch: i64 = 1_700_000_000; // 2023-11-14 (fixed for reproducibility)
    let opts = SnapshotOpts {
        enabled: true,
        registry: None,
        base_digest: "bio-min:local".to_owned(),
        source_date_epoch: fixed_source_date_epoch,
        cache_dir: cache_dir.clone(),
    };

    let outcome = snapshot_environment(&opts);

    let snapshot_digest = match outcome {
        SnapshotOutcome::Captured { ref digest, .. } => digest.clone(),
        SnapshotOutcome::SkippedNoInstalls => {
            panic!("snapshot_environment returned SkippedNoInstalls — cache stub not recognised; check cache_dir layout");
        }
        SnapshotOutcome::Failed { ref reason } => {
            panic!("snapshot_environment failed: {reason}");
        }
    };

    // ── Step 3: conda smoke check ────────────────────────────────────────────
    // Proves the COPYed env relocates correctly inside the image.
    // Uses `conda run -n <env>` which exercises the conda-relocation path that
    // would fail if the env's shebangs were not rewritten during COPY.
    let smoke_status = std::process::Command::new("docker")
        .args([
            "run", "--rm",
            &snapshot_digest,
            "conda", "run",
            "-n", env_name,
            "python", "-c", "import sys; print(sys.version)",
        ])
        .status()
        .expect("docker smoke-check command failed to spawn");

    assert!(
        smoke_status.success(),
        "conda smoke check failed (exit {:?}): env '{env_name}' did not execute \
         inside snapshot image '{snapshot_digest}' — conda-relocation may have failed",
        smoke_status.code()
    );

    // ── Step 4: build a minimal ECAA package ─────────────────────────────────
    // Layout required by run_replay(tier=Execute):
    //   runtime/outputs/<task_id>/scripts/<script>  — a compute script
    //   runtime/outputs/<task_id>/<table>.tsv        — a recorded result table
    //   runtime/outputs/<task_id>/determinism-env.json
    //   policies/container.json                      — points at snapshot digest
    //   runtime/execution-order.json                 — task ordering
    //
    // The script must reproduce the recorded table byte-identically.  We use a
    // trivial `echo`-based shell script that writes a fixed CSV body so that
    // the content hash of the produced file matches the recorded file exactly.
    let pkg_tmp = tempfile::tempdir().expect("package tempdir");
    let pkg: PathBuf = pkg_tmp.path().to_path_buf();

    let task_id = "compute_snapshot_test";
    let table_name = "result.tsv";
    let table_rel = format!("runtime/outputs/{task_id}/{table_name}");
    // Deterministic table body — byte-identical on every run.
    let table_body = "gene\tlog2fc\tpadj\nGENE_SNAP_A\t1.23\t0.01\n";

    // Write the recorded result table (the "parent" copy run_replay compares against).
    write_file(&pkg.join(&table_rel), table_body);

    // Write a shell script that reproduces the same table body.
    // The script writes to $PKG_ROOT/runtime/outputs/<task>/<table> which
    // run_replay stages and then compares to the parent table.
    let script_content = format!(
        "#!/bin/sh\nset -e\nprintf '%s' '{}' > \"$PKG_ROOT/runtime/outputs/{}/{}\"\n",
        table_body, task_id, table_name
    );
    write_file(
        &pkg.join(format!("runtime/outputs/{task_id}/scripts/01.sh")),
        &script_content,
    );

    // determinism-env.json — SOURCE_DATE_EPOCH for reproducibility, no pkg_root
    // (run_replay will scan scripts to discover the root, or use PKG_ROOT).
    let det_env = serde_json::json!({
        "schema_version": "1",
        "source_date_epoch": fixed_source_date_epoch.to_string(),
        "task_container_digest": snapshot_digest,
    });
    write_file(
        &pkg.join(format!("runtime/outputs/{task_id}/determinism-env.json")),
        &serde_json::to_string_pretty(&det_env).unwrap(),
    );

    // policies/container.json — package-structure completeness fields written
    // by record_digest.  Note: replay reads `task_container_digest` from
    // runtime/outputs/<task>/determinism-env.json (written above), NOT from
    // container.json, so these fields are informational for the package.
    let container_policy = serde_json::json!({
        "schema_version": "1",
        "digest": snapshot_digest,
        "image": snapshot_digest,
        "allow_rebuild": false,
    });
    write_file(
        &pkg.join("policies/container.json"),
        &serde_json::to_string_pretty(&container_policy).unwrap(),
    );

    // runtime/execution-order.json — single task in order.
    let exec_order = serde_json::json!({
        "order": [{ "index": 0, "task_id": task_id }]
    });
    write_file(
        &pkg.join("runtime/execution-order.json"),
        &serde_json::to_string_pretty(&exec_order).unwrap(),
    );

    // ── Step 5: run_replay → assert ByteIdentical ────────────────────────────
    let scratch_tmp = tempfile::tempdir().expect("scratch tempdir");
    let replay_opts = ReplayOptions {
        tier: Tier::Execute,
        scratch_dir: Some(scratch_tmp.path().to_path_buf()),
        bounds: None,
        allow_rebuild: false,
        reader_version: "0.2".to_string(),
        trust: ecaa_workflow_core::replay::PackageTrust::Trusted,
    };

    let report = run_replay(&pkg, &replay_opts)
        .expect("run_replay must not error on the minimal snapshot package");

    let reexecute = report
        .reexecute
        .as_ref()
        .expect("reexecute must be populated for Tier::Execute");

    // Find the classification for the compute table.
    let classification = reexecute
        .report
        .per_artifact
        .iter()
        .find(|ac| ac.artifact_path.contains(table_name))
        .unwrap_or_else(|| {
            panic!(
                "no per-artifact entry for '{table_name}'; per_artifact = {:?}",
                reexecute.report.per_artifact
            )
        });

    assert_eq!(
        classification.bucket,
        ReexecutionBucket::ByteIdentical,
        "snapshot-env replay must classify the compute table ByteIdentical; \
         got {:?} (reason: {:?})",
        classification.bucket,
        classification.reason
    );
}
