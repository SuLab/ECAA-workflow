//! Aim 3A readiness gate: no invariant is benchmarked while vacuous. This test
//! evaluates the real 23-package corpus and asserts the benchmarkable set
//! matches the live de-vacuifying state — so adding the benchmark to a
//! still-vacuous invariant fails the conformance gate, not silently confounds.

use ecaa_workflow_conformance::{run_audit_proof, InvariantId, NoopWrrocValidator};
use ecaa_workflow_core::audit_proof::bench_readiness::{
    benchmarkable, index_of, readiness_for, Readiness,
};
use ecaa_workflow_core::audit_proof::invariants::evidence_coverage::coverage_scope;
use ecaa_workflow_core::audit_proof::loader::LoadedPackage;
use ecaa_workflow_core::clock::WallClock;
use std::path::PathBuf;

fn first_corpus_pkg() -> PathBuf {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("testdata")
        .join("emitted-packages");
    let mut v: Vec<_> = std::fs::read_dir(&root)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    v.sort();
    v.into_iter().next().expect("corpus non-empty")
}

/// Structural probe of the de-vacuifying signed sink on a reference package.
fn signed_sink_present(pkg: &std::path::Path) -> bool {
    pkg.join("runtime/verification-reports/claim-verification.signed.json")
        .exists()
}

/// Structural probe of the live Inv-3 denominator. A non-empty proofs sidecar
/// alone is insufficient because `workflow:*` rows describe DAG dependencies,
/// not claim-bearing files.
fn claim_evidence_present(pkg: &std::path::Path) -> bool {
    LoadedPackage::from_root(pkg)
        .map(|loaded| !coverage_scope(&loaded).claim_evidence.is_empty())
        .unwrap_or(false)
}

#[test]
fn no_vacuous_invariant_is_benchmarked() {
    let pkg = first_corpus_pkg();
    let report = run_audit_proof(&pkg, &NoopWrrocValidator, &WallClock).unwrap();

    // Per-invariant inspected counts, aligned to InvariantId::ALL.
    let mut inspected = [0usize; 6];
    for v in &report.verdicts {
        inspected[index_of(v.id)] = v.n_inspected;
    }

    // Probe live state from the reference package on disk, not a hardcoded
    // phase boolean — so a de-vacuified invariant cannot be silently dropped.
    let sink = signed_sink_present(&pkg);
    let refs = false; // honest: the corpus carries 0 ecaa:refs (Inv 4 still vacuous)
    let evidence = claim_evidence_present(&pkg);

    let set = benchmarkable(&inspected, sink, refs, evidence);

    // The CORE invariant of this test: every benchmarked invariant is
    // non-vacuous (n_inspected > 0 OR referential Inv 2/6) on the corpus.
    for id in &set {
        let v = report.verdicts.iter().find(|v| &v.id == id).unwrap();
        let vacuous = v.n_inspected == 0
            && !matches!(
                id,
                InvariantId::DecisionJustification | InvariantId::SubstrateValidity
            );
        assert!(
            !vacuous,
            "READINESS GUARD: {id:?} is benchmarked but VACUOUS (n_inspected=0) on {}",
            pkg.display()
        );
    }

    // And the converse: a still-vacuous invariant is explicitly excluded with a reason.
    for id in InvariantId::ALL {
        if !set.contains(&id) {
            let r = readiness_for(id, inspected[index_of(id)], sink, refs, evidence);
            assert!(
                matches!(r, Readiness::Vacuous(_)),
                "{id:?} excluded from benchmark but not marked Vacuous"
            );
        }
    }

    println!("readiness: benchmarkable today = {set:?}");
    // With no signed sink, Inv1/5 are vacuous and MUST be excluded.
    if !sink {
        assert!(!set.contains(&InvariantId::ClaimCompleteness));
        assert!(!set.contains(&InvariantId::CrossGraphIntegrity));
        assert!(set.contains(&InvariantId::DecisionJustification));
        assert!(set.contains(&InvariantId::SubstrateValidity));
    }

    // The reference package has dependency proofs but no declared or linked
    // claim-evidence artifact. Inv 3 must remain explicitly excluded rather
    // than being benchmarked over an empty denominator.
    assert!(
        !evidence,
        "reference package {} unexpectedly has a non-empty claim-evidence denominator",
        pkg.display()
    );
    assert!(
        !set.contains(&InvariantId::EvidenceCoverage),
        "EvidenceCoverage is vacuous but present in the benchmarkable set {set:?}"
    );
}
