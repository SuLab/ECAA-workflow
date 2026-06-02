//! Inv-1 (claim-completeness, referential form) as runnable SHACL.
//!
//! A `Claim` is conformant iff it is `pending` OR carries ≥1 `supported_by`
//! edge. This is the SHACL second-implementation of the referential half of
//! the Rust `check_claim_completeness`. Probe-skips LOUDLY when the
//! pyld/rdflib/pyshacl toolchain is absent.

use crate::_shacl_harness::{loud_skip, run_projection, validators_available};

#[test]
fn shacl_passes_on_supported_claim() {
    if !validators_available() {
        loud_skip("shacl_passes_on_supported_claim");
        return;
    }
    let (status, stdout, stderr) = run_projection("claim-completeness-ok");
    eprintln!("--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}");
    assert!(
        status.success(),
        "supported claim must exit 0 (got {status:?})"
    );
    assert!(
        stdout.contains("SHACL conformance: PASS"),
        "supported claim must PASS:\n{stdout}"
    );
}

#[test]
fn shacl_fails_on_unsupported_nonpending_claim() {
    if !validators_available() {
        loud_skip("shacl_fails_on_unsupported_nonpending_claim");
        return;
    }
    let (status, stdout, stderr) = run_projection("claim-completeness-bad");
    eprintln!("--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}");
    assert!(
        !status.success(),
        "a non-pending claim with no supported_by must exit non-zero"
    );
    assert!(
        stdout.contains("SHACL conformance: FAIL"),
        "unsupported non-pending claim must FAIL:\n{stdout}"
    );
}
