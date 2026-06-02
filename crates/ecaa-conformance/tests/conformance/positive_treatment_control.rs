//! Aim 3A positive-treatment control (ecological leg). The benchmark must
//! DETECT real violations: every harvested-violations fixture reproduces a
//! non-Pass on a detectable invariant. Together with the null-treatment
//! control, this brackets the apparatus: quiet on clean inputs, loud on
//! violating ones. The synthetic-mutator leg lives in `invariant_utility.rs`
//! (`benchmark_positive_treatment_mutators_flip`) so it can reuse the private
//! spec-derived mutators rather than duplicating them.

use ecaa_workflow_conformance::{
    run_audit_proof, InvariantId, InvariantStatus, NoopWrrocValidator,
};
use ecaa_workflow_core::clock::WallClock;
use std::path::PathBuf;

fn harvested_base() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("harvested-violations")
}

#[test]
fn harvested_violations_detected_on_benchmarkable_invariant() {
    let base = harvested_base();
    if !base.exists() {
        eprintln!("no harvested fixtures yet; skipping ecological leg");
        return;
    }
    let mut checked = 0usize;
    for entry in std::fs::read_dir(&base).unwrap().filter_map(|e| e.ok()) {
        let d = entry.path();
        if !d.is_dir() {
            continue;
        }
        let report = run_audit_proof(&d, &NoopWrrocValidator, &WallClock).unwrap();
        // At least one non-Pass on a detectable invariant. evidence_coverage is
        // the natural harvest signal today; Inv 1/5 join post-Phase-1 once the
        // signed sink populates verdicts.
        let detected = report.verdicts.iter().any(|v| {
            v.status != InvariantStatus::Pass
                && matches!(
                    v.id,
                    InvariantId::EvidenceCoverage
                        | InvariantId::ClaimCompleteness
                        | InvariantId::CrossGraphIntegrity
                        | InvariantId::EquivalenceFailure
                )
        });
        assert!(
            detected,
            "positive-treatment: harvested {} carries no detectable violation",
            d.display()
        );
        checked += 1;
    }
    println!("positive-treatment ecological: {checked} harvested fixtures all detected");
}
