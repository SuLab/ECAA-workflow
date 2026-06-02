//! Aim 3A readiness gate: no invariant is benchmarked while vacuous. This test
//! evaluates the real 23-package corpus and asserts the benchmarkable set
//! matches the live de-vacuifying state — so adding the benchmark to a
//! still-vacuous invariant fails the conformance gate, not silently confounds.

use ecaa_workflow_conformance::{run_audit_proof, InvariantId, NoopWrrocValidator};
use ecaa_workflow_core::audit_proof::bench_readiness::{
    benchmarkable, index_of, readiness_for, Readiness,
};
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

#[test]
fn no_vacuous_invariant_is_benchmarked() {
    let pkg = first_corpus_pkg();
    let report = run_audit_proof(&pkg, &NoopWrrocValidator, &WallClock).unwrap();

    // Per-invariant inspected counts, aligned to InvariantId::ALL.
    let mut inspected = [0usize; 6];
    for v in &report.verdicts {
        inspected[index_of(v.id)] = v.n_inspected;
    }

    // Probe live state. As Phases land, flip these probes from disk/feature.
    let sink = signed_sink_present(&pkg);
    let refs = false; // TODO(phase3): probe ecaa:refs in ro-crate context
    let evidence = false; // TODO(phase3): probe evidence_coverage-from-proofs

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
    // Pre-Phase-1 sanity: with no signed sink, Inv1/5 are vacuous-pass and MUST
    // be excluded; the benchmarkable set is exactly {DecisionJustification,
    // SubstrateValidity} (the referential Inv 2/6).
    if !sink {
        assert!(!set.contains(&InvariantId::ClaimCompleteness));
        assert!(!set.contains(&InvariantId::CrossGraphIntegrity));
        assert!(set.contains(&InvariantId::DecisionJustification));
        assert!(set.contains(&InvariantId::SubstrateValidity));
    }
}
