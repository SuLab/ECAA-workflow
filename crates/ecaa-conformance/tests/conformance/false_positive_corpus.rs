//! False-positive corpus + harvested-violation regression tests.
//!
//! Specificity-on-one-package (the injection matrix in `invariant_utility.rs`)
//! is not statistical specificity. This module runs the reference audit-proof
//! evaluator over the 23 real, valid, emitted packages in
//! `testdata/emitted-packages/` and asserts none of them spuriously `Fail`,
//! and that the three *structural* invariants (`claim_completeness`,
//! `equivalence_failure`, `cross_graph_integrity`) hold on every valid package.
//! That is the missing false-positive evidence: the evaluator is quiet on
//! genuinely-clean inputs.
//!
//! It also reproduces any *real* (non-synthetic) violations harvested from
//! agent/eval runs by `scripts/harvest-invariant-violations.py` into
//! `tests/fixtures/harvested-violations/`, guarding the invariants against
//! regressions on shapes that occur in practice (e.g. a natural
//! `evidence_coverage = warn`).

use ecaa_workflow_conformance::{
    run_audit_proof, InvariantId, InvariantStatus, NoopWrrocValidator,
};
use ecaa_workflow_core::clock::WallClock;
use std::path::PathBuf;

/// The 23 real emitted packages under `testdata/emitted-packages/`.
fn corpus_dirs() -> Vec<PathBuf> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("testdata")
        .join("emitted-packages");
    let mut v: Vec<_> = std::fs::read_dir(&root)
        .unwrap_or_else(|e| panic!("read corpus dir {}: {e}", root.display()))
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    v.sort();
    v
}

#[test]
fn no_spurious_fail_on_valid_emitted_corpus() {
    let dirs = corpus_dirs();
    assert!(
        dirs.len() >= 23,
        "expected >=23 corpus packages, found {}",
        dirs.len()
    );

    for d in &dirs {
        let report = run_audit_proof(d, &NoopWrrocValidator, &WallClock)
            .unwrap_or_else(|e| panic!("audit-proof failed on {}: {e}", d.display()));

        // (a) No blocking invariant fires on a valid, emitted package. A `Fail`
        //     blocks emission, so a `Fail` here would be a genuine false positive.
        //     (`InvariantVerdict` exposes `id`/`status`/`detail`; there is no
        //     `id_str()` helper, so we format the id with `{:?}`.)
        for v in &report.verdicts {
            assert_ne!(
                v.status,
                InvariantStatus::Fail,
                "FALSE POSITIVE: {} -> {:?} = Fail (detail {:?})",
                d.display(),
                v.id,
                v.detail
            );
        }

        // (b) The three structural invariants must Pass on every valid package.
        //     (decision_justification and evidence_coverage are intentionally
        //     Unverified/Warn on the corpus — see the uniform corpus signature
        //     (pass, unverified, warn, pass, pass, unverified) — so they are
        //     excluded from this structural-Pass assertion.)
        for id in [
            InvariantId::ClaimCompleteness,
            InvariantId::EquivalenceFailure,
            InvariantId::CrossGraphIntegrity,
        ] {
            let s = report
                .verdicts
                .iter()
                .find(|v| v.id == id)
                .unwrap_or_else(|| panic!("{} missing invariant {id:?}", d.display()))
                .status;
            assert_eq!(
                s,
                InvariantStatus::Pass,
                "{} -> {id:?} expected Pass on a valid package, got {s:?}",
                d.display()
            );
        }
    }

    println!(
        "false-positive corpus: {} packages, 0 spurious Fail, 3/3 structural invariants Pass on each",
        dirs.len()
    );
}

/// Regression over the harvested-violation fixtures produced by
/// `scripts/harvest-invariant-violations.py`. Each fixture carries an
/// `EXPECTED.json` mapping the debug-formatted invariant id (e.g.
/// `"EvidenceCoverage"`) to the observed status string (e.g. `"warn"`); we
/// re-run the evaluator and assert the observed verdicts reproduce.
///
/// If the harvester has not yet captured anything (no fixtures on disk), the
/// test prints a notice and returns — it does not fail, because real non-`Pass`
/// packages are rare (a `Fail` blocks emission) and may be absent on a given
/// checkout.
#[test]
fn harvested_violations_reproduce_expected_verdicts() {
    let base = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("harvested-violations");
    if !base.exists() {
        eprintln!("no harvested-violations fixtures yet; skipping");
        return;
    }

    let mut fixtures = 0usize;
    for entry in std::fs::read_dir(&base)
        .unwrap_or_else(|e| panic!("read harvested-violations dir: {e}"))
        .filter_map(|e| e.ok())
    {
        let d = entry.path();
        if !d.is_dir() {
            continue;
        }
        let expected_path = d.join("EXPECTED.json");
        if !expected_path.exists() {
            panic!(
                "harvested fixture {} has no EXPECTED.json",
                d.display()
            );
        }
        let expected: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(&expected_path)
                .unwrap_or_else(|e| panic!("read {}: {e}", expected_path.display())),
        )
        .unwrap_or_else(|e| panic!("parse {}: {e}", expected_path.display()));

        let report = run_audit_proof(&d, &NoopWrrocValidator, &WallClock)
            .unwrap_or_else(|e| panic!("audit-proof failed on harvested {}: {e}", d.display()));

        for v in &report.verdicts {
            // EXPECTED.json keys are the debug-formatted ids (`{:?}`), values
            // are lowercase status strings. Compare case-insensitively.
            if let Some(want) = expected.get(format!("{:?}", v.id)) {
                assert_eq!(
                    want.as_str().unwrap().to_lowercase(),
                    format!("{:?}", v.status).to_lowercase(),
                    "harvested {} -> {:?}: expected {want}, got {:?}",
                    d.display(),
                    v.id,
                    v.status
                );
            }
        }
        fixtures += 1;
        println!("harvested fixture reproduced: {}", d.display());
    }

    if fixtures == 0 {
        eprintln!(
            "harvested-violations dir exists but contains 0 fixtures; nothing to reproduce"
        );
    } else {
        println!("harvested-violations: {fixtures} fixture(s) reproduced their observed verdicts");
    }
}
