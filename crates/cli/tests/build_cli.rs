//! CLI integration test: `scripps-workflow build`.
//!
//! `build --help` smoke-tests the CLI surface without requiring a
//! fixture. A full `build --archetype <yaml>` run is hard to drive
//! deterministically without staging an archetype YAML; the
//! Makefile's `make e2e` covers that path.

use assert_cmd::Command;
use predicates::str;

#[test]
fn build_help_succeeds() {
    Command::cargo_bin("scripps-workflow")
        .expect("cargo bin scripps-workflow")
        .args(["build", "--help"])
        .assert()
        .success()
        .stdout(str::contains("--archetype"))
        .stdout(str::contains("--output"));
}
