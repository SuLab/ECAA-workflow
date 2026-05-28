//! Integration tests for the closed-enum slot manifest (Proposal B).
//!
//! Validates (1) the SlotManifest YAML shape parses, (2) the registry
//! loads `*.slots.yaml` sidecars onto `ArchetypeDefinition.slots`, and
//! (3) `resolve_slot_value` keyword matching picks the first matching
//! value (or falls back to default).

use scripps_workflow_core::archetype_slots::{SlotManifest, SlotValue};

#[test]
fn slot_manifest_parses_from_yaml() {
    let yaml = r#"
slot_name: integrator
slot_kind: closed_enum
default: generic
values:
  - id: diablo
    keywords: [diablo, "spls-da", mixomics]
    extra_atoms:
      - { atom_id: validate_sample_alignment_n_way, alias: cross_omics_alignment_check, required: true,
          depends_on: [rnaseq_quantification, proteomics_protein_quantification] }
      - { atom_id: integrate_multi_omics_diablo, alias: cross_omics_diablo_integration, required: true,
          depends_on: [cross_omics_alignment_check] }
      - { atom_id: final_reporting, alias: cross_omics_diablo_final_reporting, required: true,
          depends_on: [cross_omics_diablo_integration] }
  - id: generic
    keywords: []
    extra_atoms: []
"#;
    let manifest: SlotManifest = serde_yml::from_str(yaml).unwrap();
    assert_eq!(manifest.slot_name, "integrator");
    assert_eq!(manifest.values.len(), 2);
    assert_eq!(manifest.default, "generic");
    let diablo = manifest.values.iter().find(|v| v.id == "diablo").unwrap();
    assert_eq!(diablo.extra_atoms.len(), 3);
}

#[test]
fn registry_loads_slot_sidecar() {
    use scripps_workflow_core::archetype_registry::ArchetypeRegistry;
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path();
    std::fs::write(
        p.join("foo.yaml"),
        r#"
schema_version: "0.1"
id: foo
version: "1.0.0"
description: test
sme_summary: test
goal_data: "data:0951"
project_class: bioinformatics
atoms:
  - atom_id: data_acquisition
    alias: data_acquisition
    required: true
    depends_on: []
slot_mappings: {}
"#,
    )
    .unwrap();
    std::fs::write(
        p.join("foo.slots.yaml"),
        r#"
slot_name: integrator
slot_kind: closed_enum
default: generic
values:
  - { id: diablo, keywords: [diablo], extra_atoms: [] }
  - { id: generic, keywords: [], extra_atoms: [] }
"#,
    )
    .unwrap();
    let reg = ArchetypeRegistry::load_from_dir(p).unwrap();
    let arch = reg.get("foo").unwrap();
    let slots = arch.slots.as_ref().expect("slots attached");
    assert_eq!(slots.slot_name, "integrator");
    assert_eq!(slots.values.len(), 2);
}

#[test]
fn slot_sidecar_does_not_load_as_primary_archetype() {
    // A `.slots.yaml` file with no matching primary archetype must not
    // become its own archetype. The filter excludes `.slots.yaml` from
    // the primary-archetype scan.
    use scripps_workflow_core::archetype_registry::ArchetypeRegistry;
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path();
    // Orphan sidecar — no corresponding orphan.yaml.
    std::fs::write(
        p.join("orphan.slots.yaml"),
        r#"
slot_name: protocol
slot_kind: closed_enum
default: generic
values:
  - { id: generic, keywords: [], extra_atoms: [] }
"#,
    )
    .unwrap();
    let reg = ArchetypeRegistry::load_from_dir(p).unwrap();
    assert!(
        reg.get("orphan").is_none(),
        "slot sidecar must not register as primary archetype"
    );
    assert!(
        reg.get("orphan.slots").is_none(),
        "slot sidecar stem must not register"
    );
}

#[test]
fn slot_value_keyword_default_roundtrips() {
    // A bare generic SlotValue with empty keywords/extra_atoms should
    // parse cleanly — the default value carries no triggers.
    let yaml = r#"
slot_name: protocol
slot_kind: closed_enum
default: generic
values:
  - { id: generic, keywords: [], extra_atoms: [] }
"#;
    let manifest: SlotManifest = serde_yml::from_str(yaml).unwrap();
    assert_eq!(manifest.values.len(), 1);
    let v: &SlotValue = &manifest.values[0];
    assert!(v.keywords.is_empty());
    assert!(v.extra_atoms.is_empty());
}

#[test]
fn resolve_slot_picks_keyword_match() {
    use scripps_workflow_core::archetype_slots::{resolve_slot_value, SlotManifest};
    let m: SlotManifest = serde_yml::from_str(
        r#"
slot_name: integrator
slot_kind: closed_enum
default: generic
values:
  - { id: diablo,  keywords: [diablo, "spls-da", mixomics], extra_atoms: [] }
  - { id: mofa,    keywords: [mofa, "factor decomposition"], extra_atoms: [] }
  - { id: generic, keywords: [], extra_atoms: [] }
"#,
    )
    .unwrap();
    assert_eq!(
        resolve_slot_value(&m, "we want DIABLO integration"),
        "diablo"
    );
    assert_eq!(resolve_slot_value(&m, "MOFA factor decomposition"), "mofa");
    assert_eq!(resolve_slot_value(&m, "join the omics layers"), "generic");
}

#[test]
fn expand_atoms_appends_slot_extras() {
    use scripps_workflow_core::archetype::ArchetypeAtomRef;
    use scripps_workflow_core::archetype_slots::{expand_atoms, SlotManifest};
    let m: SlotManifest = serde_yml::from_str(
        r#"
slot_name: integrator
slot_kind: closed_enum
default: generic
values:
  - { id: diablo,  keywords: [diablo], extra_atoms: [{ atom_id: integrate_multi_omics_diablo, required: true }] }
  - { id: generic, keywords: [], extra_atoms: [] }
"#,
    )
    .unwrap();
    let base = vec![ArchetypeAtomRef {
        atom_id: "data_acquisition".into(),
        alias: None,
        depends_on: vec![],
        required: true,
        required_figures: None,
        plot_stage_id: None,
        figure_exempt: None,
        expected_artifacts: None,
        required_artifacts: None,
    }];
    let expanded = expand_atoms(&base, &m, "diablo");
    assert_eq!(expanded.len(), 2);
    assert_eq!(expanded[0].atom_id.as_str(), "data_acquisition");
    assert_eq!(expanded[1].atom_id.as_str(), "integrate_multi_omics_diablo");

    // generic adds nothing.
    let generic = expand_atoms(&base, &m, "generic");
    assert_eq!(generic.len(), 1);
}
