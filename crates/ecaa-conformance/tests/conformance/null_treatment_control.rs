//! Aim 3A null-treatment control. On a CLEAN package (no claims, no Mismatch)
//! the apparatus has nothing to act on, so arm A (enforcement on) and arm B'
//! (ECAA_ABLATE_CLAIM_CONSISTENCY) must produce IDENTICAL benchmarkable
//! verdicts. A non-zero cross-arm delta here is a confound, not a signal — the
//! detector for the residual status-enum channel (design §10 R6).

use ecaa_workflow_conformance::{
    run_audit_proof, InvariantId, InvariantStatus, NoopWrrocValidator,
};
use ecaa_workflow_core::clock::WallClock;
use std::path::PathBuf;

fn corpus_dirs() -> Vec<PathBuf> {
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
    v
}

fn signature(dir: &std::path::Path) -> Vec<(InvariantId, InvariantStatus)> {
    let r = run_audit_proof(dir, &NoopWrrocValidator, &WallClock).unwrap();
    let mut s: Vec<_> = r.verdicts.iter().map(|v| (v.id, v.status)).collect();
    s.sort_by_key(|(id, _)| format!("{id:?}"));
    s
}

#[test]
#[serial_test::serial]
fn clean_corpus_scores_identically_across_arms() {
    let dirs = corpus_dirs();
    assert!(dirs.len() >= 23, "expected >=23 clean packages");
    for d in &dirs {
        // Arm A: enforcement on.
        std::env::remove_var("ECAA_ABLATE_CLAIM_CONSISTENCY");
        let arm_a = signature(d);
        // Arm B': claim-consistency ablated.
        std::env::set_var("ECAA_ABLATE_CLAIM_CONSISTENCY", "1");
        let arm_b = signature(d);
        std::env::remove_var("ECAA_ABLATE_CLAIM_CONSISTENCY");

        assert_eq!(
            arm_a,
            arm_b,
            "NULL-TREATMENT CONFOUND: clean package {} scored differently across arms \
             (A={arm_a:?} B'={arm_b:?}) — residual status-enum channel, run the R6 triage protocol",
            d.display()
        );
    }
    println!(
        "null-treatment: {} clean packages, 0 cross-arm deltas",
        dirs.len()
    );
}
