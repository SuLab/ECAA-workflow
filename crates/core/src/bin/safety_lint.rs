//! `safety-lint` — run the atom-safety lint over config/stage-atoms/
//! and print any violations. Used by `make safety-lint` and CI.
//!
//! Exit codes:
//! - 0: all atoms pass the safety lint
//! - 1: lint violations
//! - 2: registry load failed (bad ECAA_CONFIG_DIR, missing files, etc.)
//!
//! Note: `AtomRegistry::load_from_dir` deliberately treats a missing
//! directory as an empty registry (composer fallback). For an
//! operator-facing lint that's a silent pass which masks misconfig, so
//! this binary explicitly verifies the directory exists and is non-empty
//! before delegating, and exits 2 with a pointer to ECAA_CONFIG_DIR.
//!
//! Scope: this binary runs ONLY the safety lint (`validate_atom_safety`
//! per-atom), not the broader `AtomRegistry::validate_consistency`.
//! Structural validation (dangling deps, version shape, method_choice
//! references) is exercised by `cargo test -p ecaa-workflow-core`.

use std::path::PathBuf;

use ecaa_workflow_core::atom_registry::AtomRegistry;
use ecaa_workflow_core::atom_safety::validate_atom_safety;

fn main() {
    let config_dir = std::env::var("ECAA_CONFIG_DIR").unwrap_or_else(|_| "./config".into());
    let stage_atoms = PathBuf::from(&config_dir).join("stage-atoms");
    eprintln!("loading atoms from {}", stage_atoms.display());

    if !stage_atoms.is_dir() {
        eprintln!(
            "✗ failed to load atom registry: {} is not a directory \
             (set ECAA_CONFIG_DIR to the config root)",
            stage_atoms.display()
        );
        std::process::exit(2);
    }

    let registry = match AtomRegistry::load_from_dir(&stage_atoms) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("✗ failed to load atom registry: {e}");
            std::process::exit(2);
        }
    };
    if registry.is_empty() {
        eprintln!(
            "✗ failed to load atom registry: no atoms found under {} \
             (check ECAA_CONFIG_DIR)",
            stage_atoms.display()
        );
        std::process::exit(2);
    }

    let mut violations = Vec::new();
    for (_id, atom) in registry.iter() {
        violations.extend(validate_atom_safety(atom));
    }

    if violations.is_empty() {
        eprintln!("✓ all {} atoms pass safety lint", registry.len());
        std::process::exit(0);
    } else {
        eprintln!("✗ safety lint failed ({} violations):", violations.len());
        for v in &violations {
            eprintln!("  - {v}");
        }
        std::process::exit(1);
    }
}
