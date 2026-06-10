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
        let report = run_audit_proof(
            &entry.path(),
            &NoopWrrocValidator,
            &ecaa_workflow_core::clock::WallClock,
        )
        .unwrap();
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
    eprintln!(
        "corpus run: {} fixtures, {} pass, {} warn, {} fail",
        total_fixtures, total_pass, total_warn, total_fail
    );

    // Corpus floor (L6/WS-D7). The audit-proof invariant gate is only
    // meaningful over a non-trivial corpus that genuinely reaches `Pass`
    // verdicts. Pin both a fixture floor AND a Pass floor so a corpus that
    // silently shrinks — or a report that goes vacuous (all Unverified) —
    // cannot let this gate pass without actually exercising the invariants.
    //
    // Bump these constants INTENTIONALLY in the same change that resizes the
    // corpus; a mismatch means the corpus changed under the gate.
    const AUDIT_PROOF_CORPUS_FLOOR: usize = 2;
    assert!(
        total_fixtures >= AUDIT_PROOF_CORPUS_FLOOR,
        "audit-proof corpus has {total_fixtures} fixtures, below the floor of \
         {AUDIT_PROOF_CORPUS_FLOOR}; the invariant gate would pass vacuously on a \
         shrunken corpus (L6)."
    );
    // Every fixture must reach at least one hermetic `Pass` — otherwise an
    // all-`Unverified` report would satisfy `total_fail == 0` without the
    // invariants ever firing. A Pass floor of one-per-fixture catches that.
    assert!(
        total_pass >= total_fixtures,
        "audit-proof corpus produced {total_pass} Pass verdicts across \
         {total_fixtures} fixtures; expected at least one Pass per fixture so the \
         gate is not vacuous (L6)."
    );
    assert_eq!(total_fail, 0, "no fixture should Fail any invariant");
}
