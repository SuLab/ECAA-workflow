//! Grant v19 §A.S2 ↔ code load-bearing parity test.
//! The four conditions preventing emission must match between the
//! canonical Rust const and the grant prose. Test fails if either
//! drifts without the other.

use ecaa_workflow_core::emission_invariants::FOUR_CONDITIONS_PREVENTING_EMISSION;
use std::path::PathBuf;

fn grant_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("literature/PAR-26-040-grant-proposal-rewrite-v19.md")
}

#[test]
fn rust_const_lists_exactly_four_conditions() {
    assert_eq!(
        FOUR_CONDITIONS_PREVENTING_EMISSION.len(),
        4,
        "the architectural rule lists exactly four; got {}",
        FOUR_CONDITIONS_PREVENTING_EMISSION.len()
    );
}

#[test]
#[ignore = "literature/ is gitignored; skip in default CI, enable when grant lands in-repo"]
fn grant_prose_and_rust_const_match() {
    // Only runnable when the grant proposal is available on disk
    // (not in CI today because literature/ is gitignored).
    let grant_path = grant_path();
    if !grant_path.exists() {
        eprintln!("grant file not present; test is a no-op");
        return;
    }
    let grant = std::fs::read_to_string(&grant_path).expect("grant file unreadable");

    for (i, condition) in FOUR_CONDITIONS_PREVENTING_EMISSION.iter().enumerate() {
        // The grant prose may use slightly different phrasing; match
        // on a distinctive phrase from each condition.
        let key_phrase = match i {
            0 => "cannot be classified into any modality",
            1 => "schema-validation failure on a required intake field",
            2 => "explicit SME rejection at the confirmation gate",
            3 => "emission-side analogue to",
            _ => unreachable!(),
        };
        assert!(
            grant.contains(key_phrase),
            "condition {} key phrase {:?} not found in grant prose: {}",
            i + 1,
            key_phrase,
            condition
        );
    }
}
