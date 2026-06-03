//! P4 — concrete estimated_duration + deterministic runtime_class→seconds.

use ecaa_workflow_core::atom::{
    runtime_class_to_seconds, AtomDefinition, DurationBasis, DurationEstimate,
};
use ecaa_workflow_core::atom_registry::AtomRegistry;
use std::path::{Path, PathBuf};

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/atoms_p4")
}

#[test]
fn runtime_class_projection_is_exhaustive_and_deterministic() {
    assert_eq!(runtime_class_to_seconds("seconds"), Some(30));
    assert_eq!(runtime_class_to_seconds("minutes"), Some(600));
    assert_eq!(runtime_class_to_seconds("hours"), Some(7200));
    assert_eq!(runtime_class_to_seconds("days"), Some(172_800));
    // Unknown bucket → None (caller falls back, never panics).
    assert_eq!(runtime_class_to_seconds("fortnights"), None);
    // Pure + deterministic: same input, same output.
    assert_eq!(
        runtime_class_to_seconds("hours"),
        runtime_class_to_seconds("hours")
    );
}

#[test]
fn duration_estimate_round_trips() {
    let mut atom = AtomDefinition::test_default("dur_atom");
    atom.estimated_duration = Some(DurationEstimate {
        seconds_p50: 600,
        seconds_p95: Some(1800),
        basis: DurationBasis::AuthorEstimate,
    });
    let json = serde_json::to_string(&atom).unwrap();
    let back: AtomDefinition = serde_json::from_str(&json).unwrap();
    let est = back.estimated_duration.expect("estimate present");
    assert_eq!(est.seconds_p50, 600);
    assert_eq!(est.seconds_p95, Some(1800));
    assert_eq!(est.basis, DurationBasis::AuthorEstimate);
}

#[test]
fn atom_with_estimated_duration_loads_from_registry() {
    let reg = AtomRegistry::load_from_dir(&fixtures_dir()).expect("fixture loads");
    let atom = reg.get("dur_atom").unwrap();
    assert_eq!(atom.estimated_duration.as_ref().unwrap().seconds_p50, 600);
}

#[test]
fn estimated_duration_missing_basis_is_rejected() {
    let bad = fixtures_dir().join("..").join("atoms_p4_bad");
    let err = AtomRegistry::load_from_dir(&bad)
        .expect_err("schema must reject estimated_duration without basis");
    assert!(format!("{err:#}").contains("failed schema validation"));
}
