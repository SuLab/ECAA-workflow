//! Non-vacuous SHACL gate (C5 Bar §8.5).
//!
//! Before this gate existed, `project_package.py` ran pyshacl over a graph
//! whose emitted nodes carried no `@type`, so every shape (`MethodChoice`,
//! `RerunOutcome`, `Package`) had zero focus nodes and pyshacl reported
//! `conforms=True` trivially. A non-conformant package could never *fail*.
//!
//! These two tests pin the gate against that vacuity:
//!
//!   * `shacl_fails_on_unjustified_method_choice` — a package with a
//!     `MethodChoice` that has neither a `cites` edge nor a ≥30-char
//!     `rationale` MUST exit non-zero with `SHACL conformance: FAIL`.
//!   * `shacl_passes_on_justified_method_choice_with_focus_nodes` — the same
//!     `MethodChoice` with a `cites` edge to a `Citation` MUST pass, and the
//!     projection MUST emit >0 triples (the guard that would have caught the
//!     zero-triples vacuity).
//!
//! Both fixtures are hand-authored (`tests/fixtures/shacl-{unjustified,
//! justified}/`) and carry the 6-IRI `ro-crate-metadata.json` so
//! `SubstrateValidityShape` (Invariant 6) has a focus node. Hand-authoring
//! decouples this gate from the C1 typed-node projection so it can land first.
//!
//! The tests probe-skip (early `return`, not a failure) when Python or the
//! `pyld` / `rdflib` / `pyshacl` deps are absent, so the suite is dispatch-safe
//! on a machine without the validator toolchain.

use std::path::PathBuf;
use std::process::Command;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("CARGO_MANIFEST_DIR has two ancestors")
        .to_path_buf()
}

fn project_script() -> PathBuf {
    repo_root()
        .join("scripts")
        .join("spec-check")
        .join("project_package.py")
}

fn fixture_dir(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name)
}

/// Returns `true` when `python3` plus `pyld`, `rdflib`, and `pyshacl` are all
/// importable. On any failure (no python, missing dep, spawn error) returns
/// `false` so the caller can probe-skip.
fn python_validators_available() -> bool {
    let out = Command::new("python3")
        .args(["-c", "import pyld, rdflib, pyshacl"])
        .output();
    match out {
        Ok(o) => o.status.success(),
        Err(_) => false,
    }
}

/// Run `project_package.py <fixture>` and return (exit-code, stdout, stderr).
fn run_projection(fixture: &str) -> (Option<i32>, String, String) {
    let output = Command::new("python3")
        .arg(project_script())
        .arg(fixture_dir(fixture))
        .output()
        .expect("spawning project_package.py");
    (
        output.status.code(),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

#[test]
fn shacl_fails_on_unjustified_method_choice() {
    if !python_validators_available() {
        eprintln!(
            "shacl_fails_on_unjustified_method_choice skipped: python3 + pyld/rdflib/pyshacl not available"
        );
        return;
    }

    let (code, stdout, stderr) = run_projection("shacl-unjustified");
    eprintln!("--- project_package.py stdout ---\n{stdout}");
    eprintln!("--- project_package.py stderr ---\n{stderr}");

    assert_eq!(
        code,
        Some(1),
        "unjustified MethodChoice must make project_package.py exit 1 (got {code:?}); \
         a zero exit means SHACL passed vacuously"
    );
    assert!(
        stdout.contains("SHACL conformance: FAIL"),
        "expected 'SHACL conformance: FAIL' in stdout; got:\n{stdout}"
    );
}

#[test]
fn shacl_passes_on_justified_method_choice_with_focus_nodes() {
    if !python_validators_available() {
        eprintln!(
            "shacl_passes_on_justified_method_choice_with_focus_nodes skipped: python3 + pyld/rdflib/pyshacl not available"
        );
        return;
    }

    let (code, stdout, stderr) = run_projection("shacl-justified");
    eprintln!("--- project_package.py stdout ---\n{stdout}");
    eprintln!("--- project_package.py stderr ---\n{stderr}");

    assert_eq!(
        code,
        Some(0),
        "justified MethodChoice (cites edge) must make project_package.py exit 0 (got {code:?})"
    );
    assert!(
        stdout.contains("SHACL conformance: PASS"),
        "expected 'SHACL conformance: PASS' in stdout; got:\n{stdout}"
    );

    // Non-vacuity guard: the projection must produce real triples. The
    // zero-triples graph is exactly the state that masked the original bug.
    let triples = parse_triple_count(&stdout)
        .unwrap_or_else(|| panic!("could not parse 'projected: N RDF triples' from:\n{stdout}"));
    assert!(
        triples > 0,
        "projection must emit >0 triples (got {triples}); zero triples is the vacuity that \
         hid the SHACL bug"
    );
}

/// Parse the `projected: <N> RDF triples` line `project_package.py` prints.
fn parse_triple_count(stdout: &str) -> Option<usize> {
    for line in stdout.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("projected:") {
            let n: String = rest.trim().chars().take_while(|c| c.is_ascii_digit()).collect();
            if let Ok(v) = n.parse::<usize>() {
                return Some(v);
            }
        }
    }
    None
}
