//! Shared helpers for the per-invariant SHACL second-impl conformance gates.
//!
//! Every `*_shacl.rs` gate runs `scripts/spec-check/project_package.py` over a
//! hand-authored fixture and asserts the global `SHACL conformance: PASS|FAIL`
//! line. The probe (`validators_available`) gates a LOUD skip when the
//! `pyld`/`rdflib`/`pyshacl` toolchain is absent, so a deps-absent run can
//! never be mistaken for a real SHACL pass.

#![allow(dead_code)]

use std::path::PathBuf;
use std::process::Command;

/// Operator-facing install hint reused by every probe-skip notice.
pub(crate) const VALIDATOR_INSTALL_HINT: &str =
    "pip install --user --break-system-packages pyshacl pyld owlready2 rdflib runcrate";

pub(crate) fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("CARGO_MANIFEST_DIR has two ancestors")
        .to_path_buf()
}

pub(crate) fn project_script() -> PathBuf {
    repo_root()
        .join("scripts")
        .join("spec-check")
        .join("project_package.py")
}

pub(crate) fn fixture_dir(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name)
}

/// `true` when `python3` plus `pyld`, `rdflib`, and `pyshacl` are importable.
pub(crate) fn validators_available() -> bool {
    Command::new("python3")
        .args(["-c", "import pyld, rdflib, pyshacl"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Run `project_package.py <fixture>` → (exit-status, stdout, stderr).
pub(crate) fn run_projection(fixture: &str) -> (std::process::ExitStatus, String, String) {
    let output = Command::new("python3")
        .arg(project_script())
        .arg(fixture_dir(fixture))
        .output()
        .expect("spawning project_package.py");
    (
        output.status,
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

/// Print a LOUD probe-skip notice naming the test that did not run.
pub(crate) fn loud_skip(test_name: &str) {
    eprintln!(
        "\n>>> SKIP: pyld/rdflib/pyshacl not importable under python3 <<<\n\
         >>> {test_name} did NOT run — this is NOT a SHACL pass. <<<\n\
         >>> Install the validator toolchain to run this gate for real:\n\
         >>>   {VALIDATOR_INSTALL_HINT}\n"
    );
}

/// Parse the `projected: <N> RDF triples` line.
pub(crate) fn parse_triple_count(stdout: &str) -> Option<usize> {
    for line in stdout.lines() {
        if let Some(rest) = line.trim().strip_prefix("projected:") {
            let n: String = rest
                .trim()
                .chars()
                .take_while(|c| c.is_ascii_digit())
                .collect();
            if let Ok(v) = n.parse::<usize>() {
                return Some(v);
            }
        }
    }
    None
}
