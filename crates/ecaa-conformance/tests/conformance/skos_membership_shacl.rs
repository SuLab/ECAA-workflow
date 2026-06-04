//! Executed enum-membership SHACL gate (F10 — makes the membership SHACL live).
//!
//! `docs/ecaa-spec/registration/ecaa-skos-membership.shacl.ttl` is deliberately
//! NOT loaded into the live `project_package.py` validate() call: that gate's
//! conformance ABox carries `Blocker.kind` as the Invariant-4 carve-out string
//! (`UnprovableEdge`/`PolicyException`) and `RerunOutcome.class` as an IRI
//! individual (`ecaa:failed`), neither of which is the snake_case
//! `skos:notation` wire token the membership SPARQL matches on — so loading them
//! there would mis-fire on the Invariant-4 fixtures.
//!
//! This gate closes that gap WITHOUT touching the live pipeline: it shells out
//! to `scripts/spec-check/test_skos_membership.py`, which projects a snake_case
//! ABox through the canonical `ecaa-v0.2.jsonld` context and runs REAL pyshacl
//! over the published membership shapes + published SKOS schemes:
//!
//!   * a REGISTERED token (`agent_error` / `byte_identical`) → the package
//!     CONFORMS, and the membership shape bound a focus node (non-vacuous);
//!   * an UNREGISTERED token (`agnt_error` / `totally_made_up`) → the
//!     membership shape FIRES (non-conformance).
//!
//! This is the SPARQL-executing counterpart to the pure-Rust string-parse lint
//! `skos_scheme_agreement.rs` (which checks Rust-enum ⇄ SKOS-scheme set
//! agreement, runs NO pyshacl/SPARQL): the two are complementary, not
//! redundant.
//!
//! The test probe-skips (early `return`, not a failure) when Python or the
//! `pyld`/`rdflib`/`pyshacl` deps are absent, printed LOUDLY so a deps-absent
//! run can never be mistaken for a real validation pass. Install with:
//!
//! ```text
//! pip install --user --break-system-packages pyshacl pyld owlready2 rdflib runcrate
//! ```

use std::path::PathBuf;
use std::process::Command;

const VALIDATOR_INSTALL_HINT: &str =
    "pip install --user --break-system-packages pyshacl pyld owlready2 rdflib runcrate";

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("CARGO_MANIFEST_DIR has two ancestors")
        .to_path_buf()
}

fn membership_gate_script() -> PathBuf {
    repo_root()
        .join("scripts")
        .join("spec-check")
        .join("test_skos_membership.py")
}

/// `true` when `python3` plus `pyld`, `rdflib`, and `pyshacl` are importable.
fn python_validators_available() -> bool {
    Command::new("python3")
        .args(["-c", "import pyld, rdflib, pyshacl"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Run the membership gate → (exit-code, stdout, stderr).
fn run_membership_gate() -> (Option<i32>, String, String) {
    let output = Command::new("python3")
        .arg(membership_gate_script())
        .output()
        .expect("spawning test_skos_membership.py");
    (
        output.status.code(),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

fn loud_skip(test_name: &str) {
    eprintln!(
        "\n>>> SKIP: pyld/rdflib/pyshacl not importable under python3 <<<\n\
         >>> {test_name} did NOT run — this is NOT a SHACL pass. <<<\n\
         >>> Install the validator toolchain to run this gate for real:\n\
         >>>   {VALIDATOR_INSTALL_HINT}\n"
    );
}

/// The membership SHACL is EXECUTED: a registered token conforms (non-vacuously)
/// and an unregistered token in each scheme makes its membership shape FIRE.
#[test]
fn skos_membership_shacl_is_executed_and_discriminating() {
    if !python_validators_available() {
        loud_skip("skos_membership_shacl_is_executed_and_discriminating");
        return;
    }

    let (code, stdout, stderr) = run_membership_gate();
    eprintln!("--- test_skos_membership.py stdout ---\n{stdout}");
    eprintln!("--- test_skos_membership.py stderr ---\n{stderr}");

    // Registered token → the package conforms (the registered-case check also
    // asserts the membership shapes bind focus nodes, i.e. non-vacuity).
    assert!(
        stdout.contains("SKOS-MEMBERSHIP: registered=PASS"),
        "registered SKOS token must CONFORM under the membership SHACL; got:\n{stdout}"
    );

    // Unregistered token in each scheme → its membership shape FIRES. An
    // `=PASS` here would mean the out-of-vocabulary token wrongly conformed —
    // exactly the F10 defect this gate exists to catch.
    assert!(
        stdout.contains("SKOS-MEMBERSHIP: unregistered-blocker=FIRES"),
        "an unregistered Blocker.kind must FIRE BlockerKindMembershipShape; got:\n{stdout}"
    );
    assert!(
        stdout.contains("SKOS-MEMBERSHIP: unregistered-rerun=FIRES"),
        "an unregistered RerunOutcome.class must FIRE RerunOutcomeClassMembershipShape; got:\n{stdout}"
    );

    assert!(
        stdout.contains("SKOS-MEMBERSHIP: gate=PASS"),
        "membership gate must report gate=PASS; got:\n{stdout}"
    );
    assert_eq!(
        code,
        Some(0),
        "membership gate must exit 0 (got {code:?}); a non-zero exit means a membership \
         assertion failed — read the stderr above"
    );
}
