//! Pins `edam_labels::edam_label` against the live archetype config tree.
//!
//! `ecaa-workflow list archetypes --json` renders a human EDAM label for each
//! archetype's `goal_data` / `goal_format` CURIE via the curated
//! `edam_labels::edam_label` map. If a new archetype introduces a `goal_data`
//! or `goal_format` CURIE the map does not cover, the catalog would silently
//! render "N/A" for a real term — this gate fails instead, forcing the label
//! to be added deliberately.

use ecaa_workflow_core::archetype_registry::ArchetypeRegistry;
use ecaa_workflow_core::edam_labels::edam_label;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

fn config_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("config")
}

#[test]
fn every_archetype_goal_iri_has_a_label() {
    let reg = ArchetypeRegistry::load_from_dir(&config_root().join("archetypes"))
        .expect("ArchetypeRegistry::load_from_dir must succeed for config/archetypes/");

    // Collect the distinct non-empty goal_data / goal_format CURIEs in use.
    let mut iris: BTreeSet<String> = BTreeSet::new();
    for (_, a) in reg.iter() {
        if !a.goal_data.trim().is_empty() {
            iris.insert(a.goal_data.clone());
        }
        if let Some(fmt) = a.goal_format.as_deref() {
            if !fmt.trim().is_empty() {
                iris.insert(fmt.to_string());
            }
        }
    }

    let unmapped: Vec<&String> = iris.iter().filter(|iri| edam_label(iri).is_none()).collect();
    assert!(
        unmapped.is_empty(),
        "archetype goal CURIEs with no edam_labels::edam_label entry: {unmapped:?}. \
         Add each to crates/core/src/edam_labels.rs with its canonical EDAM \
         preferred label in the same change that introduced the archetype."
    );
}
