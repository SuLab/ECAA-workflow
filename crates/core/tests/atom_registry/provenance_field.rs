//! P2 — provenance { origin, maintainer } on AtomDefinition.

use ecaa_workflow_core::atom::{AtomDefinition, AtomOrigin, AtomProvenance};
use ecaa_workflow_core::atom_registry::AtomRegistry;
use std::path::{Path, PathBuf};

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/atoms_p2")
}

#[test]
fn provenance_round_trips() {
    let mut atom = AtomDefinition::test_default("prov_atom");
    atom.provenance = Some(AtomProvenance {
        origin: AtomOrigin::Builtin,
        maintainer: "scripps-bioinformatics".into(),
    });
    let json = serde_json::to_string(&atom).unwrap();
    let back: AtomDefinition = serde_json::from_str(&json).unwrap();
    let prov = back.provenance.expect("provenance present");
    assert_eq!(prov.origin, AtomOrigin::Builtin);
    assert_eq!(prov.maintainer, "scripps-bioinformatics");
}

#[test]
fn atom_with_provenance_loads_from_registry() {
    let reg = AtomRegistry::load_from_dir(&fixtures_dir()).expect("fixture loads");
    let atom = reg.get("prov_atom").unwrap();
    let prov = atom.provenance.as_ref().expect("provenance present");
    assert_eq!(prov.origin, AtomOrigin::SiteLocal);
}

#[test]
fn provenance_missing_maintainer_is_rejected() {
    let bad = fixtures_dir().join("..").join("atoms_p2_bad");
    let err = AtomRegistry::load_from_dir(&bad)
        .expect_err("schema must reject provenance without maintainer");
    assert!(format!("{err:#}").contains("failed schema validation"));
}
