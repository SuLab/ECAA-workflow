//! CLI integration test: `scripps-workflow chat`.
//!
//! The deterministic chat REPL is line-oriented and would require a
//! scripted stdin fixture to drive end-to-end; that flow is exercised
//! by `make ivd-chat`. This file scopes to argument-parsing surface
//! coverage so a regression in `clap` wiring fails the unit-test
//! grid rather than waiting for the scenario harness.

use assert_cmd::Command;
use predicates::str;

#[test]
fn chat_help_succeeds() {
    Command::cargo_bin("scripps-workflow")
        .expect("cargo bin scripps-workflow")
        .args(["chat", "--help"])
        .assert()
        .success()
        .stdout(str::contains("--config"))
        .stdout(str::contains("--output"));
}

#[test]
fn top_level_help_lists_subcommands() {
    Command::cargo_bin("scripps-workflow")
        .expect("cargo bin scripps-workflow")
        .arg("--help")
        .assert()
        .success()
        .stdout(str::contains("chat"))
        .stdout(str::contains("intake"))
        .stdout(str::contains("build"))
        .stdout(str::contains("dag"));
}
