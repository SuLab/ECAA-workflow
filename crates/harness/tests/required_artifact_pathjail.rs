//! Regression: required_artifacts paths must stay inside the task output
//! directory.

use scripps_workflow_core::dag::RequiredArtifact;
use scripps_workflow_harness::required_artifacts::verify_required_artifacts;

fn artifact(path: &str) -> RequiredArtifact {
    RequiredArtifact {
        path: path.to_string(),
        min_size_bytes: None,
        schema_ref: None,
        validation_obligations: vec![],
    }
}

#[test]
fn rejects_absolute_path() {
    let tmp = tempfile::tempdir().unwrap();
    let err = verify_required_artifacts(tmp.path(), "task", &[artifact("/etc/passwd")])
        .expect_err("absolute path should reject");
    assert!(err.to_string().contains("absolute"), "{err}");
}

#[test]
fn rejects_parent_relative_path() {
    let tmp = tempfile::tempdir().unwrap();
    let err = verify_required_artifacts(tmp.path(), "task", &[artifact("../escape.txt")])
        .expect_err("parent-relative path should reject");
    assert!(err.to_string().contains("parent"), "{err}");
}

#[test]
fn accepts_normal_relative_path() {
    let tmp = tempfile::tempdir().unwrap();
    let output_dir = tmp.path().join("runtime/outputs/task");
    std::fs::create_dir_all(&output_dir).unwrap();
    std::fs::write(output_dir.join("ok.txt"), b"hi").unwrap();
    let missing =
        verify_required_artifacts(tmp.path(), "task", &[artifact("ok.txt")]).expect("normal path");
    assert!(
        missing.is_empty(),
        "expected no missing artifacts, got {missing:?}"
    );
}
