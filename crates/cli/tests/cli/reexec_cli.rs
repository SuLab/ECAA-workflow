//! CLI integration test: `ecaa-workflow reexec`.
//!
//! Stages two tempdir packages — a parent with a result table under
//! `runtime/outputs/` and a byte-identical replay — then drives `reexec`
//! and asserts that `runtime/reexecution.json` is written with a non-empty
//! `per_artifact`. Also covers the `--into` redirect and the help surface.

use assert_cmd::Command;
use predicates::str;

/// Write a file, creating parent dirs first.
fn write_file(path: &std::path::Path, contents: &str) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("create parent dirs");
    }
    std::fs::write(path, contents).expect("write fixture file");
}

#[test]
fn reexec_help_succeeds() {
    Command::cargo_bin("ecaa-workflow")
        .expect("cargo bin ecaa-workflow")
        .args(["reexec", "--help"])
        .assert()
        .success()
        .stdout(str::contains("--parent"))
        .stdout(str::contains("--replay"));
}

#[test]
fn reexec_writes_report_with_nonempty_per_artifact() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let parent = tmp.path().join("parent");
    let replay = tmp.path().join("replay");
    let rel = "runtime/outputs/differential_expression/de_results.tsv";
    let body = "gene\tlog2fc\tpadj\nGENE1\t1.5\t0.01\nGENE2\t-2.0\t0.04\n";
    write_file(&parent.join(rel), body);
    // Byte-identical replay copy → every artifact passes → exit 0.
    write_file(&replay.join(rel), body);

    Command::cargo_bin("ecaa-workflow")
        .expect("cargo bin ecaa-workflow")
        .args([
            "reexec",
            "--parent",
            parent.to_str().unwrap(),
            "--replay",
            replay.to_str().unwrap(),
        ])
        .assert()
        .success();

    // Report written under --replay (no --into given).
    let report_path = replay.join("runtime").join("reexecution.json");
    let raw = std::fs::read_to_string(&report_path)
        .unwrap_or_else(|e| panic!("read {}: {e}", report_path.display()));
    let json: serde_json::Value =
        serde_json::from_str(&raw).expect("reexecution.json is valid JSON");
    let per_artifact = json
        .get("per_artifact")
        .and_then(|v| v.as_array())
        .expect("per_artifact array present");
    assert!(
        !per_artifact.is_empty(),
        "per_artifact must be non-empty, got: {raw}"
    );
    assert_eq!(
        per_artifact[0].get("bucket").and_then(|b| b.as_str()),
        Some("byte_identical"),
        "identical table must be byte_identical, got: {raw}"
    );
}

#[test]
fn reexec_into_redirects_report_destination() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let parent = tmp.path().join("parent");
    let replay = tmp.path().join("replay");
    let into = tmp.path().join("into-pkg");
    let rel = "results/tables/x.tsv";
    let body = "gene\tvalue\nGENE1\t42\n";
    write_file(&parent.join(rel), body);
    write_file(&replay.join(rel), body);

    Command::cargo_bin("ecaa-workflow")
        .expect("cargo bin ecaa-workflow")
        .args([
            "reexec",
            "--parent",
            parent.to_str().unwrap(),
            "--replay",
            replay.to_str().unwrap(),
            "--into",
            into.to_str().unwrap(),
        ])
        .assert()
        .success();

    // Report lands under --into, not --replay.
    assert!(
        into.join("runtime").join("reexecution.json").exists(),
        "report must be written under --into"
    );
    assert!(
        !replay.join("runtime").join("reexecution.json").exists(),
        "report must NOT be written under --replay when --into is given"
    );
}

#[test]
fn reexec_no_tables_exits_nonzero() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let parent = tmp.path().join("parent");
    let replay = tmp.path().join("replay");
    // Neither package has any result tables → empty report → non-zero exit.
    std::fs::create_dir_all(&parent).expect("mkdir parent");
    std::fs::create_dir_all(&replay).expect("mkdir replay");

    Command::cargo_bin("ecaa-workflow")
        .expect("cargo bin ecaa-workflow")
        .args([
            "reexec",
            "--parent",
            parent.to_str().unwrap(),
            "--replay",
            replay.to_str().unwrap(),
        ])
        .assert()
        .failure();
}
