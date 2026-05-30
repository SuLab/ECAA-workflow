//! Every
//! production atom must convert to a `TaskNode` without panicking
//! and round-trip through serde without information loss.
//!
//! This test is the load-bearing contract that lets later phases
//! consume `TaskNode`-shaped data uniformly across atom-derived
//! and freshly-authored nodes. If a new atom YAML adds a field
//! the converter doesn't yet preserve, this test fails until
//! `from_atom.rs::preserve_attributes` is updated.

use std::collections::BTreeMap;
use std::path::Path;

use ecaa_workflow_core::atom_registry::AtomRegistry;
use ecaa_workflow_core::workflow_contracts::{
    Implementation, LifecycleState, NodeStatus, TaskNode, TrustLevel,
};

fn repo_root() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}

fn registry() -> AtomRegistry {
    let dir = repo_root().join("config/stage-atoms");
    AtomRegistry::load_from_dir(&dir).expect("load stage atoms")
}

#[test]
fn every_atom_converts_to_task_node() {
    let registry = registry();
    assert!(!registry.is_empty(), "atom registry is empty");

    for (id, atom) in registry.iter() {
        let node = TaskNode::from_atom(atom);
        assert_eq!(&node.id, id, "id should match");
        assert_eq!(node.machine_name, *id, "machine_name should match id");
        assert!(
            !node.intent.is_empty(),
            "atom {id} converts with empty intent"
        );
        // Migrated atoms always start at Contracted/Unverified per policy.
        assert!(
            matches!(node.lifecycle_state, LifecycleState::Contracted),
            "atom {id} should default to Contracted lifecycle, got {:?}",
            node.lifecycle_state
        );
        assert!(matches!(node.trust_level, TrustLevel::Unverified));
        assert!(matches!(node.status, NodeStatus::Active));
    }
}

#[test]
fn every_converted_node_round_trips_through_json() {
    let registry = registry();
    for (id, atom) in registry.iter() {
        let node = TaskNode::from_atom(atom);
        let json =
            serde_json::to_string(&node).unwrap_or_else(|e| panic!("atom {id} serialize: {e}"));
        let back: TaskNode =
            serde_json::from_str(&json).unwrap_or_else(|e| panic!("atom {id} deserialize: {e}"));
        assert_eq!(
            node, back,
            "atom {id} round-trip changed shape (left=before, right=after)"
        );
    }
}

#[test]
fn every_converted_node_round_trips_through_yaml() {
    let registry = registry();
    for (id, atom) in registry.iter() {
        let node = TaskNode::from_atom(atom);
        let yaml =
            serde_yml::to_string(&node).unwrap_or_else(|e| panic!("atom {id} serialize: {e}"));
        let back: TaskNode =
            serde_yml::from_str(&yaml).unwrap_or_else(|e| panic!("atom {id} deserialize: {e}"));
        assert_eq!(node, back, "atom {id} YAML round-trip changed shape");
    }
}

#[test]
fn semver_parses_for_every_atom() {
    let registry = registry();
    for (id, atom) in registry.iter() {
        let node = TaskNode::from_atom(atom);
        // SemVer parsing is permissive (falls back to 0.0.0 on
        // garbage), but we expect every production atom to carry
        // a valid version string. This test pins the contract.
        assert_eq!(
            node.version.render(),
            atom.version,
            "atom {id} version round-trip changed: original={} render={}",
            atom.version,
            node.version.render()
        );
    }
}

#[test]
fn sme_atoms_become_manual_protocol_implementations() {
    let registry = registry();
    let mut sme_count = 0;
    for (_id, atom) in registry.iter() {
        let node = TaskNode::from_atom(atom);
        match (&atom.assignee, &node.implementation) {
            (
                ecaa_workflow_core::atom::AtomAssignee::Sme,
                Implementation::ManualProtocol { .. },
            ) => {
                sme_count += 1;
            }
            (ecaa_workflow_core::atom::AtomAssignee::Sme, other) => {
                panic!(
                    "SME-assignee atom {} converted to {:?}, expected ManualProtocol",
                    atom.id, other
                );
            }
            _ => {}
        }
    }
    // We don't assert a specific count because the registry
    // evolves; this just guarantees the contract holds for every
    // SME atom that *does* exist.
    let _ = sme_count;
}

#[test]
fn agent_atoms_with_container_become_container_command() {
    let registry = registry();
    for (_id, atom) in registry.iter() {
        let node = TaskNode::from_atom(atom);
        if matches!(atom.assignee, ecaa_workflow_core::atom::AtomAssignee::Agent)
            && atom.preferred_container.is_some()
        {
            assert!(
                matches!(node.implementation, Implementation::ContainerCommand { .. }),
                "atom {} has preferred_container but didn't lower to ContainerCommand: {:?}",
                atom.id,
                node.implementation
            );
        }
    }
}

#[test]
fn attributes_bag_preserves_authoring_fields() {
    let registry = registry();
    for (id, atom) in registry.iter() {
        let node = TaskNode::from_atom(atom);
        let a = &node.attributes;

        // Role is always preserved.
        assert!(
            a.contains_key("role"),
            "atom {id} missing role in attributes bag"
        );
        // Assignee is always preserved.
        assert!(
            a.contains_key("assignee"),
            "atom {id} missing assignee in attributes bag"
        );

        if !atom.depends_on.is_empty() {
            let stored: Vec<String> = serde_json::from_value(a.get("depends_on").cloned().unwrap())
                .unwrap_or_else(|e| panic!("atom {id} depends_on roundtrip: {e}"));
            assert_eq!(stored, atom.depends_on, "atom {id} depends_on mismatch");
        }
        if !atom.excludes.is_empty() {
            let stored: Vec<String> =
                serde_json::from_value(a.get("excludes").cloned().unwrap()).unwrap();
            assert_eq!(stored, atom.excludes, "atom {id} excludes mismatch");
        }
        if let Some(c) = &atom.claim_boundary {
            assert_eq!(
                a.get("claim_boundary").and_then(|v| v.as_str()),
                Some(c.as_str()),
                "atom {id} claim_boundary mismatch"
            );
        }
        if let Some(cond) = &atom.condition {
            assert_eq!(
                a.get("condition").and_then(|v| v.as_str()),
                Some(cond.as_str()),
                "atom {id} condition mismatch"
            );
        }
    }
}

#[test]
fn provenance_records_source_atom_path() {
    let registry = registry();
    for (id, atom) in registry.iter() {
        let node = TaskNode::from_atom(atom);
        let expected = format!("config/stage-atoms/{id}.yaml");
        assert_eq!(
            node.provenance.source.as_deref(),
            Some(expected.as_str()),
            "atom {id} provenance source mismatch"
        );
    }
}

#[test]
fn deterministic_attribute_iteration() {
    // BTreeMap iteration is sorted; converting the same atom
    // twice must produce byte-identical JSON.
    let registry = registry();
    for (_id, atom) in registry.iter() {
        let node1 = TaskNode::from_atom(atom);
        let node2 = TaskNode::from_atom(atom);
        let j1 = serde_json::to_string(&node1).unwrap();
        let j2 = serde_json::to_string(&node2).unwrap();
        assert_eq!(j1, j2, "from_atom is non-deterministic");
    }
}

#[test]
fn aggregator_atoms_have_no_input_ports() {
    let registry = registry();
    for (id, atom) in registry.iter() {
        let node = TaskNode::from_atom(atom);
        if matches!(
            atom.role.default_behavior_class(),
            ecaa_workflow_core::atom::AtomRole::Aggregator
        ) {
            assert!(
                node.inputs.is_empty(),
                "aggregator atom {id} has inputs after conversion: {:?}",
                node.inputs
            );
        }
    }
}

#[test]
fn unknown_atom_attributes_are_preserved_unchanged() {
    // If an atom has an `attributes` map with custom keys, those
    // keys should appear unchanged in the TaskNode's attributes.
    let registry = registry();
    let mut tested = 0;
    for (id, atom) in registry.iter() {
        if atom.attributes.is_empty() {
            continue;
        }
        let node = TaskNode::from_atom(atom);
        let a: BTreeMap<String, serde_json::Value> = node.attributes;
        for (k, v) in &atom.attributes {
            // The synthesized keys (`role`, `assignee`, etc.) take
            // priority and may shadow author keys; we only check
            // that author keys NOT in the reserved set survive.
            const RESERVED: &[&str] = &[
                "role",
                "discovery_kind",
                "assignee",
                "depends_on",
                "excludes",
                "joint_with",
                "method_choice",
                "resource_profile",
                "preferred_container",
                "claim_boundary",
                "condition",
                "iterate",
                "required_figures",
                "plot_stage_id",
                "expected_artifacts",
                "required_artifacts",
                "runtime_packages",
            ];
            if RESERVED.contains(&k.as_str()) {
                continue;
            }
            assert_eq!(a.get(k), Some(v), "atom {id} attribute {k} not preserved");
            tested += 1;
        }
    }
    let _ = tested;
}
