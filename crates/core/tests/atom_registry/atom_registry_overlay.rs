//! Integration tests for
//! [`ecaa_workflow_core::atom_registry::AtomRegistry::with_promoted_overlay`].
//!
//! The overlay constructor stitches
//! synthesized AtomDefinitions onto a base registry without mutating
//! it, so the v4 composer's planner can pick up Promoted proposals as
//! legitimate candidates. The contract is narrow:
//!
//! 1. Overlay atoms appear in the returned registry alongside base
//! atoms.
//! 2. Id collisions resolve in favor of the base registry — a
//! promotion can never silently shadow a production atom.
//! 3. The overlay-introduced marker attribute survives the
//! construction so downstream code can distinguish overlay atoms
//! from registry atoms.
//!
//! The base registry is materialized via a tempdir + handwritten YAML
//! so the test exercises the real `load_from_dir` + schema path,
//! matching how `try_build_via_composer` invokes the constructor in
//! production.

use ecaa_workflow_core::atom::{AtomAssignee, AtomDefinition, AtomRole};
use ecaa_workflow_core::atom_registry::AtomRegistry;
use std::collections::BTreeMap;
use std::io::Write;
use std::path::Path;

fn write_atom(dir: &Path, name: &str, body: &str) {
    let p = dir.join(format!("{}.yaml", name));
    let mut f = std::fs::File::create(&p).unwrap();
    f.write_all(body.as_bytes()).unwrap();
}

fn base_registry_with(id: &str) -> AtomRegistry {
    let tmp = tempfile::tempdir().unwrap();
    write_atom(
        tmp.path(),
        id,
        &format!(
            r#"id: {}
version: "1.0.0"
role: operation
description: "Test base atom."
edam_operation: operation:0292
edam_data: data:2978
edam_format: format:2572
assignee: agent
"#,
            id
        ),
    );
    AtomRegistry::load_from_dir(tmp.path()).unwrap()
}

/// Build a synthesized overlay atom carrying the `_proposal_overlay`
/// marker so the third test below can assert it survives the
/// constructor.
fn overlay_atom(id: &str) -> AtomDefinition {
    let mut attributes: BTreeMap<String, serde_json::Value> = BTreeMap::new();
    attributes.insert("_proposal_overlay".to_string(), serde_json::json!(true));
    AtomDefinition {
        id: id.to_string(),
        version: "0.0.0".to_string(),
        role: AtomRole::Operation,
        discovery_kind: None,
        description: format!("Synthesized overlay atom for {id}"),
        edam_operation: "ecaax:proposal_test".to_string(),
        edam_data: None,
        edam_format: None,
        assignee: AtomAssignee::Agent,
        depends_on: Vec::new(),
        excludes: Vec::new(),
        attributes,
        joint_with: Vec::new(),
        inputs: Vec::new(),
        outputs: Vec::new(),
        method_choice: None,
        resource_profile: None,
        preferred_container: None,
        claim_boundary: None,
        iterate: None,
        condition: None,
        required_figures: Vec::new(),
        plot_stage_id: None,
        figure_exempt: None,
        expected_artifacts: Vec::new(),
        required_artifacts: Vec::new(),
        validators: Vec::new(),
        runtime_packages: Default::default(),
        safety: Default::default(),
    }
}

#[test]
fn with_promoted_overlay_adds_new_atom() {
    let base = base_registry_with("base_atom");
    assert_eq!(base.len(), 1);
    assert!(base.get("base_atom").is_some());
    assert!(base.get("overlay_atom").is_none());

    let merged = base.with_promoted_overlay(vec![overlay_atom("overlay_atom")]);

    // The returned registry contains BOTH the base atom AND the overlay atom.
    assert_eq!(merged.len(), 2, "merged registry must carry both atoms");
    assert!(
        merged.get("base_atom").is_some(),
        "base atom must survive the overlay construction"
    );
    assert!(
        merged.get("overlay_atom").is_some(),
        "overlay atom must appear in the merged registry"
    );

    // The source registry is left unchanged — `with_promoted_overlay`
    // returns a fresh registry without mutating the receiver.
    assert_eq!(base.len(), 1, "base registry must NOT be mutated");
    assert!(
        base.get("overlay_atom").is_none(),
        "overlay atom must NOT leak back into the base"
    );
}

#[test]
fn overlay_skips_collisions_with_base_registry() {
    // Build a base with a single atom, then attempt to overlay an
    // atom that uses the SAME id but a different description. The
    // collision MUST resolve in favor of the base registry (production
    // atom wins; the overlay can never silently shadow it).
    let base = base_registry_with("conflicting_id");
    let base_description = base
        .get("conflicting_id")
        .expect("base must have the atom")
        .description
        .clone();

    let mut overlay = overlay_atom("conflicting_id");
    overlay.description = "SHOULD NOT WIN — overlay must lose to base".to_string();
    let merged = base.with_promoted_overlay(vec![overlay]);

    // The merged registry's atom at the collided id is the BASE atom,
    // NOT the overlay's.
    assert_eq!(merged.len(), 1, "collision must not duplicate the row");
    let resolved = merged
        .get("conflicting_id")
        .expect("merged must still carry the colliding id");
    assert_eq!(
        resolved.description, base_description,
        "base atom must win the id collision; overlay description must NOT replace it"
    );
}

#[test]
fn overlay_marker_attribute_present() {
    // Build a base registry that doesn't carry the marker, overlay a
    // synthesized atom that DOES, and assert the marker survives the
    // constructor on the overlay-introduced row.
    let base = base_registry_with("base_no_marker");
    let merged = base.with_promoted_overlay(vec![overlay_atom("marked_overlay")]);

    let marked = merged
        .get("marked_overlay")
        .expect("overlay-introduced atom must be retrievable post-merge");
    assert_eq!(
        marked
            .attributes
            .get("_proposal_overlay")
            .and_then(|v| v.as_bool()),
        Some(true),
        "_proposal_overlay marker must persist after with_promoted_overlay"
    );

    // The base atom must NOT have grown the marker (overlay does not
    // bleed into pre-existing rows).
    let base_atom = merged.get("base_no_marker").unwrap();
    assert!(
        !base_atom.attributes.contains_key("_proposal_overlay"),
        "base atom must NOT acquire the overlay marker"
    );
}
