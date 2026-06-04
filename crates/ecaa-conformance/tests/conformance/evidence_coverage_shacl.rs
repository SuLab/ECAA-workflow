//! Inv-3 (evidence-coverage) as runnable SHACL — second-impl gate.
//!
//! A package whose V `OutputFile` is referenced by a Claim `supported_by`
//! edge PASSES; one with a dangling, un-acknowledged output FAILS. This is the
//! SHACL second-implementation of the Rust `check_evidence_coverage` and the
//! standards half of F6. Probe-skips LOUDLY when the pyld/rdflib/pyshacl
//! toolchain is absent — a skip is never a vacuous pass.

use crate::_shacl_harness::{loud_skip, parse_triple_count, run_projection, validators_available};

#[test]
fn shacl_passes_on_covered_output() {
    if !validators_available() {
        loud_skip("shacl_passes_on_covered_output");
        return;
    }
    let (status, stdout, stderr) = run_projection("evidence-coverage-covered");
    eprintln!("--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}");
    assert!(
        status.success(),
        "covered output must make project_package.py exit 0 (got {status:?})"
    );
    assert!(
        stdout.contains("SHACL conformance: PASS"),
        "covered output must PASS:\n{stdout}"
    );
    // Non-vacuity: the projection must emit real triples (OutputFile + Claim).
    let triples = parse_triple_count(&stdout)
        .unwrap_or_else(|| panic!("could not parse 'projected: N RDF triples':\n{stdout}"));
    assert!(
        triples > 0,
        "projection must emit >0 triples (got {triples})"
    );
}

#[test]
fn shacl_fails_on_uncovered_output() {
    if !validators_available() {
        loud_skip("shacl_fails_on_uncovered_output");
        return;
    }
    let (status, stdout, stderr) = run_projection("evidence-coverage-uncovered");
    eprintln!("--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}");
    assert!(
        !status.success(),
        "uncovered output must make project_package.py exit non-zero"
    );
    assert!(
        stdout.contains("SHACL conformance: FAIL"),
        "uncovered output must FAIL:\n{stdout}"
    );
}
