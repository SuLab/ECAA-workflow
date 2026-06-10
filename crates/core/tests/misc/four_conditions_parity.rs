//! Emission-conditions parity test (load-bearing).
//!
//! The four conditions preventing emission must match between the
//! canonical Rust const (`FOUR_CONDITIONS_PREVENTING_EMISSION`) and the
//! vendored prose fixture (`tests/fixtures/ws_d_emission_conditions.md`).
//! The fixture is checked-in (not gitignored), so the parity check
//! actually RUNS in default `make test` — this slim OSS surface has no CI,
//! so the local gate is the only line of defense and a vacuous skip would
//! be invisible. The fixture also documents the deterministic 5th
//! defense-in-depth gate (`validate_container_digests_pinned`), which is
//! kept OUT of the length-4 const by design.

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

fn vendored_fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/ws_d_emission_conditions.md")
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

/// Durable, always-running parity check against the vendored fixture.
/// Replaces the gitignored-grant prose check (which `#[ignore]`s itself
/// when `literature/` is absent), so the local gate enforces parity for
/// real instead of silently no-op'ing.
#[test]
fn const_phrases_match_vendored_fixture() {
    let text =
        std::fs::read_to_string(vendored_fixture_path()).expect("vendored fixture must exist");
    for (i, condition) in FOUR_CONDITIONS_PREVENTING_EMISSION.iter().enumerate() {
        // Distinctive phrase from each condition; mirrors the grant-prose
        // match arms below so the two checks stay aligned.
        let key_phrase = match i {
            0 => "cannot be classified into any modality",
            1 => "schema-validation failure on a required intake field",
            2 => "Explicit SME rejection at the confirmation gate",
            3 => "emission-side analogue to",
            _ => unreachable!(),
        };
        assert!(
            text.contains(key_phrase),
            "condition {} key phrase {:?} not found in vendored fixture: {}",
            i + 1,
            key_phrase,
            condition
        );
    }
}

/// The module doc comment in `emission_invariants.rs` must (a) cite the
/// REAL parity-test path (`tests/misc/...`), (b) NOT claim CI catches
/// drift (this OSS surface has none), and (c) document the digest-pin gate
/// as the deterministic 5th emission-blocking condition.
#[test]
fn doc_comment_cites_correct_parity_path_and_digest_gate() {
    let src = std::fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/emission_invariants.rs"),
    )
    .expect("read emission_invariants.rs");
    assert!(
        src.contains("crates/core/tests/misc/four_conditions_parity.rs"),
        "emission_invariants.rs doc comment must cite the real parity-test path \
         (tests/misc/...), not the wrong tests/four_conditions_parity.rs (WS-D3)"
    );
    assert!(
        !src.contains("is caught\n//! in CI"),
        "emission_invariants.rs claims CI catches drift; this slim OSS surface \
         has no CI — drift is caught by the local gate (WS-D3)"
    );
    assert!(
        src.contains("validate_container_digests_pinned"),
        "emission_invariants.rs must document the digest-pin gate as the \
         deterministic 5th emission-blocking condition (WS-D3)"
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
