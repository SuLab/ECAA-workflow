//! Asserts the migration is complete — no atom carries
//! figure_exempt.category == "pending_affordance_migration".
//! Runs as part of `make test`.

use std::path::Path;

use ecaa_workflow_core::atom_registry::AtomRegistry;

#[test]
fn no_atom_carries_pending_affordance_migration() {
    let atom_dir = Path::new("../../config/stage-atoms");
    let atoms = AtomRegistry::load_from_dir(atom_dir).expect("load atoms");
    let offenders: Vec<String> = atoms
        .iter()
        .filter_map(|(_, a)| {
            a.figure_exempt.as_ref().and_then(|fe| {
                if fe.category.as_deref() == Some("pending_affordance_migration") {
                    Some(a.id.clone())
                } else {
                    None
                }
            })
        })
        .collect();
    assert!(
        offenders.is_empty(),
        "atoms still on pending_affordance_migration: {:?}",
        offenders
    );
}
