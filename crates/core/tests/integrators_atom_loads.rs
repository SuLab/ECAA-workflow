//! Three integrative multi-omics atoms (DIABLO, MOFA, SNF) plus the
//! `discover_integration_method` discovery atom round-trip through the
//! atom registry, and the base cross-omics archetype carries an
//! `integrator` slot that supplies DIABLO/MOFA/SNF/generic variants.

use scripps_workflow_core::archetype_registry::ArchetypeRegistry;
use scripps_workflow_core::atom::AtomRole;
use scripps_workflow_core::atom_registry::AtomRegistry;
use std::path::PathBuf;

fn config_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("config")
}

#[test]
fn diablo_mofa_snf_atoms_load_round_trip() {
    let reg = AtomRegistry::load_from_dir(&config_root().join("stage-atoms"))
        .expect("AtomRegistry must load");
    for atom_id in [
        "integrate_multi_omics_diablo",
        "integrate_multi_omics_mofa",
        "integrate_multi_omics_snf",
    ] {
        let atom = reg
            .get(atom_id)
            .unwrap_or_else(|| panic!("integrator atom {atom_id} must be registered"));
        assert_eq!(
            atom.role,
            AtomRole::Operation,
            "{atom_id} must be Operation role"
        );
        assert!(
            atom.preferred_container.is_some(),
            "{atom_id} must declare a preferred_container (derived image)"
        );
        assert!(
            atom.claim_boundary.as_ref().is_some_and(|s| !s.is_empty()),
            "{atom_id} must declare a non-empty claim_boundary"
        );
    }
}

#[test]
fn discover_integration_method_atom_loads() {
    let reg = AtomRegistry::load_from_dir(&config_root().join("stage-atoms"))
        .expect("AtomRegistry must load");
    let atom = reg
        .get("discover_integration_method")
        .expect("discover_integration_method must be registered");
    assert_eq!(atom.role, AtomRole::Discovery);
    assert!(
        atom.discovery_kind.as_deref() == Some("integration_method"),
        "discovery_kind must be 'integration_method', got {:?}",
        atom.discovery_kind
    );
}

#[test]
fn cross_omics_rnaseq_proteomics_carries_integrator_slot() {
    // Slot-fill replaces the per-integrator archetype variants. The
    // base `cross_omics_rnaseq_proteomics` archetype now declares a
    // slot manifest with closed-enum integrator ∈ {diablo, mofa, snf,
    // generic}; expand_atoms appends the chosen value's extra_atoms at
    // composition time.
    let reg = ArchetypeRegistry::load_from_dir(&config_root().join("archetypes"))
        .expect("ArchetypeRegistry must load");
    let arch = reg
        .get("cross_omics_rnaseq_proteomics")
        .expect("base cross_omics_rnaseq_proteomics must be registered");
    let slots = arch
        .slots
        .as_ref()
        .expect("base archetype must carry an integrator slot manifest");
    assert_eq!(slots.slot_name, "integrator");
    assert_eq!(slots.default, "generic");
    let ids: std::collections::HashSet<&str> = slots.values.iter().map(|v| v.id.as_str()).collect();
    for required in ["diablo", "mofa", "snf", "generic"] {
        assert!(
            ids.contains(required),
            "integrator slot must declare {required} value; got {:?}",
            ids
        );
    }
}

#[test]
fn integrator_slot_values_pin_distinct_extra_atoms() {
    // Each named integrator value pins its own integration atom in
    // extra_atoms; without distinct extra-atom sets the variants
    // would collapse onto identical compositions.
    let reg = ArchetypeRegistry::load_from_dir(&config_root().join("archetypes"))
        .expect("ArchetypeRegistry must load");
    let arch = reg.get("cross_omics_rnaseq_proteomics").unwrap();
    let slots = arch.slots.as_ref().unwrap();
    for (value_id, expected_atom) in [
        ("diablo", "integrate_multi_omics_diablo"),
        ("mofa", "integrate_multi_omics_mofa"),
        ("snf", "integrate_multi_omics_snf"),
    ] {
        let v = slots.values.iter().find(|v| v.id == value_id).unwrap();
        assert!(
            v.extra_atoms
                .iter()
                .any(|a| a.atom_id.as_str() == expected_atom),
            "integrator value {value_id} must include {expected_atom} in extra_atoms; \
             got {:?}",
            v.extra_atoms.iter().map(|a| &a.atom_id).collect::<Vec<_>>()
        );
    }
}
