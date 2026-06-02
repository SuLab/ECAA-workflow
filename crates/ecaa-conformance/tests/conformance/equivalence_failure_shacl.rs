//! Inv-4 (equivalence-failure) SHACL binding via the projected `refs` edge.
//!
//! The `EquivalenceFailureShape` SPARQL was already in the SHACL file, but no
//! projector emitted the `refs` Blocker→RerunOutcome edge it queries, so a
//! diverged-without-ack package passed vacuously (the FILTER NOT EXISTS always
//! succeeded). `_project.py` now types Q `RerunOutcome` + F `Blocker` rows and
//! projects the `refs` edge: an unacknowledged divergence FAILS, an
//! acknowledged one PASSES. Probe-skips LOUDLY when the toolchain is absent.

use crate::_shacl_harness::{loud_skip, run_projection, validators_available};

#[test]
fn shacl_fails_on_unacknowledged_divergence() {
    if !validators_available() {
        loud_skip("shacl_fails_on_unacknowledged_divergence");
        return;
    }
    let (status, stdout, stderr) = run_projection("equivalence-failure-unack");
    eprintln!("--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}");
    assert!(
        !status.success(),
        "unacknowledged divergence must FAIL Inv-4 (exit non-zero)"
    );
    assert!(
        stdout.contains("SHACL conformance: FAIL"),
        "unacknowledged divergence must FAIL:\n{stdout}"
    );
}

#[test]
fn shacl_passes_on_acknowledged_divergence() {
    if !validators_available() {
        loud_skip("shacl_passes_on_acknowledged_divergence");
        return;
    }
    let (status, stdout, stderr) = run_projection("equivalence-failure-ack");
    eprintln!("--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}");
    assert!(
        status.success(),
        "acknowledged divergence must exit 0 (got {status:?})"
    );
    assert!(
        stdout.contains("SHACL conformance: PASS"),
        "acknowledged divergence must PASS:\n{stdout}"
    );
}
