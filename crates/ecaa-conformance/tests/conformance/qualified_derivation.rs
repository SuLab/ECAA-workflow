//! Amendment-lineage projection as `prov:qualifiedDerivation` (F7-engineering).
//!
//! The reference emitter records lineage as a plain `prov:wasDerivedFrom` on
//! the root Dataset. `_project.py` now ADDITIONALLY reifies it as a
//! `prov:Derivation` node linked via `prov:qualifiedDerivation`, so a
//! second-impl can inspect the derivation structure. This test projects a
//! lineage fixture and asserts the serialized `package.ttl` carries both the
//! `prov:qualifiedDerivation` edge and a `prov:Derivation`-typed node.
//! Probe-skips LOUDLY when the toolchain is absent.

use crate::_shacl_harness::{fixture_dir, loud_skip, run_projection, validators_available};

#[test]
fn projects_qualified_derivation_node() {
    if !validators_available() {
        loud_skip("projects_qualified_derivation_node");
        return;
    }
    let (status, stdout, stderr) = run_projection("qualified-derivation");
    eprintln!("--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}");
    // The lineage fixture is otherwise conformant (6 IRIs), so it must PASS.
    assert!(
        status.success(),
        "qualified-derivation fixture must project + PASS (got {status:?})"
    );

    let ttl = std::fs::read_to_string(fixture_dir("qualified-derivation").join("package.ttl"))
        .expect("package.ttl must have been written by project_package.py");
    assert!(
        ttl.contains("prov:qualifiedDerivation"),
        "package.ttl must carry a prov:qualifiedDerivation edge:\n{ttl}"
    );
    assert!(
        ttl.contains("prov:Derivation"),
        "package.ttl must carry a prov:Derivation-typed node:\n{ttl}"
    );
}
