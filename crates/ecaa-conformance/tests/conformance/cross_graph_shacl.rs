//! Inv-5 (cross-graph-integrity) as runnable SHACL — second-impl gate.
//!
//! Every `supported_by` object must resolve to a node typed in the V
//! (Evidence) graph. A claim that supports a present `OutputFile` PASSES; one
//! that supports a dangling output IRI (no V node) FAILS. SHACL
//! second-implementation of the C→V form of `check_cross_graph_integrity`.
//! Probe-skips LOUDLY when the toolchain is absent.

use crate::_shacl_harness::{loud_skip, run_projection, validators_available};

#[test]
fn shacl_passes_on_resolved_supported_by() {
    if !validators_available() {
        loud_skip("shacl_passes_on_resolved_supported_by");
        return;
    }
    let (status, stdout, stderr) = run_projection("cross-graph-ok");
    eprintln!("--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}");
    assert!(
        status.success(),
        "resolved supported_by must exit 0 (got {status:?})"
    );
    assert!(
        stdout.contains("SHACL conformance: PASS"),
        "resolved supported_by must PASS:\n{stdout}"
    );
}

#[test]
fn shacl_fails_on_dangling_supported_by() {
    if !validators_available() {
        loud_skip("shacl_fails_on_dangling_supported_by");
        return;
    }
    let (status, stdout, stderr) = run_projection("cross-graph-dangling");
    eprintln!("--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}");
    assert!(
        !status.success(),
        "a dangling supported_by target must exit non-zero"
    );
    assert!(
        stdout.contains("SHACL conformance: FAIL"),
        "dangling supported_by must FAIL:\n{stdout}"
    );
}
