use ecaa_workflow_conformance::{run_audit_proof, InvariantStatus, NoopWrrocValidator};
use std::path::PathBuf;

#[test]
fn corpus_passes_audit_proof_or_fails_with_known_reasons() {
    let corpus: PathBuf = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("core")
        .join("tests")
        .join("fixtures")
        .join("audit-proof");
    let mut total_fixtures = 0;
    let mut total_pass = 0;
    let mut total_warn = 0;
    let mut total_fail = 0;
    for entry in std::fs::read_dir(&corpus).unwrap() {
        let entry = entry.unwrap();
        if !entry.file_type().unwrap().is_dir() {
            continue;
        }
        total_fixtures += 1;
        let report = run_audit_proof(&entry.path(), &NoopWrrocValidator).unwrap();
        for v in &report.verdicts {
            match v.status {
                InvariantStatus::Pass => total_pass += 1,
                InvariantStatus::Warn => total_warn += 1,
                InvariantStatus::Fail => total_fail += 1,
                InvariantStatus::Unverified => {}
                _ => {}
            }
        }
    }
    assert!(total_fixtures >= 1, "expected at least one fixture");
    eprintln!(
        "corpus run: {} fixtures, {} pass, {} warn, {} fail",
        total_fixtures, total_pass, total_warn, total_fail
    );
    // Document the current floor; tighten as the corpus grows.
    assert_eq!(total_fail, 0, "no fixture should Fail any invariant");
}
