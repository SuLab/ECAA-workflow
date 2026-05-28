use scripps_workflow_core::audit_proof::{run_audit_proof, InvariantStatus};
use scripps_workflow_core::wrroc_validator::NoopWrrocValidator;
use std::path::PathBuf;

fn fixture_root(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("audit-proof")
        .join(name)
}

#[test]
fn run_audit_proof_on_minimal_fixture() {
    let root = fixture_root("minimal-emitted-package");
    let report = run_audit_proof(&root, &NoopWrrocValidator).expect("ok");
    assert_eq!(report.verdicts.len(), 6);
    // Minimal fixture should pass 5 substantive invariants (or be unverified)
    let n_fail = report
        .verdicts
        .iter()
        .filter(|v| v.status == InvariantStatus::Fail)
        .count();
    assert_eq!(
        n_fail, 0,
        "minimal fixture should not Fail any invariant: {:?}",
        report.verdicts
    );
}
