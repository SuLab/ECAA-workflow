//! CLI integration test: `scripps-workflow dag`.
//!
//! `dag --help` smoke-tests the surface. Driving `dag --package` end-to-end
//! would require emitting a package first; the `intake_emits_package_for_minimal_request`
//! test in `intake_cli.rs` already exercises that emit path, so this
//! file scopes to argument-parsing surface coverage.

use assert_cmd::Command;
use predicates::str;

#[test]
fn dag_help_succeeds() {
    Command::cargo_bin("ecaa-workflow")
        .expect("cargo bin scripps-workflow")
        .args(["dag", "--help"])
        .assert()
        .success()
        .stdout(str::contains("--package"))
        .stdout(str::contains("--dot"));
}

#[test]
fn dag_lists_tasks_for_emitted_package() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let request_path = tmp.path().join("request.md");
    std::fs::write(
        &request_path,
        "Bulk RNA-seq differential expression analysis of human samples.\n",
    )
    .expect("write request fixture");

    let output_dir = tmp.path().join("output-package");
    let config_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crates/")
        .parent()
        .expect("repo root")
        .join("config");

    // First emit a package so dag has something to inspect.
    Command::cargo_bin("ecaa-workflow")
        .expect("cargo bin scripps-workflow")
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

    // dag inspection. At least the workflow id surfaces — every emitted
    // package writes a non-empty WORKFLOW.json with one or more tasks.
    Command::cargo_bin("ecaa-workflow")
        .expect("cargo bin scripps-workflow")
        .args(["dag", "--package", output_dir.to_str().unwrap()])
        .assert()
        .success();
}
