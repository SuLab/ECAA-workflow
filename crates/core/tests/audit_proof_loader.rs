use scripps_workflow_core::audit_proof::loader::LoadedPackage;
use std::path::PathBuf;

fn fixture_root(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("audit-proof")
        .join(name)
}

#[test]
fn loader_reads_existing_fixture_package() {
    let root = fixture_root("minimal-emitted-package");
    let loaded = LoadedPackage::from_root(&root).expect("fixture should load");
    // Minimum expected: at least one decision and one claim row.
    assert!(!loaded.decisions.is_empty());
    assert!(loaded.claims.is_some());
    assert!(loaded.determinism_shim.is_some());
}

#[test]
fn loader_tolerates_missing_optional_sidecars() {
    let root = fixture_root("minimal-no-affordances");
    let loaded = LoadedPackage::from_root(&root).expect("fixture should load");
    assert!(loaded.plot_affordances.is_none());
}
