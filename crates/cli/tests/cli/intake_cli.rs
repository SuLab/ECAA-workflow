//! CLI integration test: `ecaa-workflow intake`.
//!
//! Closes the coverage gap where the `intake` subcommand was
//! end-to-end tested only via the Makefile (`make ivd`). Exercises
//! `--help` (no I/O) and a real intake against a tiny request fixture
//! that should classify and emit a package directory.

use assert_cmd::Command;
use predicates::str;
use std::path::PathBuf;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crates/")
        .parent()
        .expect("repo root")
        .to_path_buf()
}

#[test]
fn intake_help_succeeds() {
    Command::cargo_bin("ecaa-workflow")
        .expect("cargo bin ecaa-workflow")
        .args(["intake", "--help"])
        .assert()
        .success()
        .stdout(str::contains("intake"))
        .stdout(str::contains("--input"))
        .stdout(str::contains("--output"));
}

#[test]
fn intake_emits_package_for_minimal_request() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let request_path = tmp.path().join("request.md");
    std::fs::write(
        &request_path,
        "Bulk RNA-seq differential expression analysis of human samples.\n\
         Identify differentially expressed genes between two conditions.\n",
    )
    .expect("write request fixture");

    let output_dir = tmp.path().join("output-package");
    let config_dir = repo_root().join("config");

    Command::cargo_bin("ecaa-workflow")
        .expect("cargo bin ecaa-workflow")
        .args([
            "intake",
            "--input",
            request_path.to_str().unwrap(),
            "--output",
            output_dir.to_str().unwrap(),
            "--config",
            config_dir.to_str().unwrap(),
        ])
        .assert()
        .success();

    assert!(
        output_dir.exists(),
        "intake did not create output dir: {}",
        output_dir.display()
    );
    assert!(
        output_dir.join("WORKFLOW.json").exists(),
        "WORKFLOW.json missing from emitted package"
    );
}
