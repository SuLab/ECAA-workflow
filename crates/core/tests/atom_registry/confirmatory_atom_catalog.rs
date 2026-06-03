//! Confirmatory-atom catalog gate.
//!
//! The recall anchor (`expected_claim::derive_expected_manifest`) decides
//! which DAG stages anchor a `Required` recall expectation (Inv 1) by
//! reading the per-atom `confirmatory` marker from the real catalog
//! (`config/stage-atoms/`), NOT a hard-coded id-substring list. This gate
//! pins the marker so a refactor can't silently strip recall anchoring:
//!
//! (a) at least one atom is marked confirmatory (the set is non-empty);
//! (b) every confirmatory atom is `role == Operation` and its id is not a
//!     `discover_*` / `validate_*` self-describing stage (illegal combos
//!     `is_confirmatory_stage` would reject anyway, caught here at the
//!     catalog level so the YAML can't carry a contradictory marker);
//! (c) a regression FLOOR: each canonical confirmatory result-producer is
//!     present in the catalog AND marked confirmatory.
//!
//! Runs as part of `make test`.

use ecaa_workflow_core::atom::AtomRole;
use ecaa_workflow_core::atom_registry::AtomRegistry;
use std::path::Path;

fn load_catalog() -> AtomRegistry {
    let atom_dir = Path::new("../../config/stage-atoms");
    AtomRegistry::load_from_dir(atom_dir).unwrap_or_else(|e| {
        panic!(
            "failed to load atom registry from {}: {e}",
            atom_dir.display()
        )
    })
}

#[test]
fn confirmatory_set_is_nonempty() {
    let reg = load_catalog();
    let confirmatory: Vec<&str> = reg
        .iter()
        .filter(|(_, a)| a.confirmatory)
        .map(|(id, _)| id.as_str())
        .collect();
    assert!(
        !confirmatory.is_empty(),
        "no atom in config/stage-atoms/ is marked `confirmatory: true` — the recall \
         anchor would never emit a Required expectation, silently disabling Inv 1's \
         recall floor. At least the canonical result-producers must carry the marker."
    );
}

#[test]
fn confirmatory_atoms_are_operations_and_not_self_describing() {
    let reg = load_catalog();
    for (id, atom) in reg.iter() {
        if !atom.confirmatory {
            continue;
        }
        assert_eq!(
            atom.role,
            AtomRole::Operation,
            "atom `{id}` is marked confirmatory but role is {:?}; only `operation` atoms \
             produce a recomputed numeric result, so a confirmatory marker on any other \
             role is a catalog authoring error.",
            atom.role
        );
        assert!(
            !id.starts_with("discover_") && !id.starts_with("validate_"),
            "atom `{id}` is marked confirmatory but is a self-describing \
             discover_*/validate_* stage; those never anchor recall expectations \
             (`is_confirmatory_stage` early-returns false), so the marker is contradictory."
        );
    }
}

#[test]
fn canonical_confirmatory_atoms_are_present_and_marked() {
    // Regression FLOOR: the recall anchor MUST keep firing for these
    // result-producing stages across refactors. Dropping the marker (or
    // the atom file) on any of these silently re-opens the under-generation
    // gap F2 closed.
    const FLOOR: &[&str] = &[
        "differential_expression",
        "variant_calling",
        "peak_calling",
        "pathway_enrichment",
        "endpoint_analysis",
        "clinical_endpoint_analysis",
    ];
    let reg = load_catalog();
    for id in FLOOR {
        let atom = reg.get(id).unwrap_or_else(|| {
            panic!(
                "canonical confirmatory atom `{id}` is absent from config/stage-atoms/ — \
                 the recall anchor's regression floor requires it to exist."
            )
        });
        assert!(
            atom.confirmatory,
            "canonical confirmatory atom `{id}` exists but is NOT marked \
             `confirmatory: true`; the recall anchor would stop emitting a Required \
             expectation for it (F2 regression)."
        );
    }
}
