use super::validation::detect_cycle;
use super::*;
use crate::goal_spec::GoalSpec;
use std::collections::BTreeMap;

/// Aggregate_resources rolls up coarse buckets into
/// numeric totals. Surfaces the breakdown the SME confirmation
/// card renders before approval.
#[test]
fn aggregate_resources_sums_buckets_and_picks_peak() {
    use crate::atom::ResourceProfile;
    let mk = |name: &str, mem: Option<&str>, cpu: Option<&str>, hours: Option<&str>, gpu: bool| {
        let mut atom = make_atom(name, AtomRole::Operation, None, None, vec![]);
        atom.resource_profile = Some(ResourceProfile {
            cpu: cpu.map(|s| s.into()),
            memory: mem.map(|s| s.into()),
            gpu,
            runtime_class: hours.map(|s| s.into()),
        });
        composed_from(atom)
    };
    let atoms = vec![
        mk(
            "light_step",
            Some("small"),
            Some("light"),
            Some("seconds"),
            false,
        ),
        mk(
            "hard_step",
            Some("xl"),
            Some("very_heavy"),
            Some("hours"),
            true,
        ),
        mk(
            "middle_step",
            Some("medium"),
            Some("moderate"),
            Some("minutes"),
            false,
        ),
    ];
    let est = aggregate_resources(&atoms);
    assert_eq!(est.total_memory_gb, 4 + 256 + 16, "memory sums in GB");
    assert_eq!(est.peak_memory_gb, 256, "peak memory is the xl atom");
    assert_eq!(est.gpu_task_count, 1);
    // 2 cores × 0.01 hr + 64 cores × 4 hr + 8 cores × 0.5 hr = 260.02
    assert!(
        (est.total_core_hours - 260.02).abs() < 1e-6,
        "core-hours total: {}",
        est.total_core_hours
    );
    assert!(est.estimated_cost_usd.is_none(), "cost left to pilot");
}

/// Atoms without a resource_profile contribute zero.
/// Conservative — the SME sees ballpark numbers, not panics.
#[test]
fn aggregate_resources_handles_atoms_without_profile() {
    let atoms = vec![composed_from(make_atom(
        "no_profile",
        AtomRole::Operation,
        None,
        None,
        vec![],
    ))];
    let est = aggregate_resources(&atoms);
    assert_eq!(est, ResourceEstimate::default());
}

fn live_atoms() -> AtomRegistry {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("config/stage-atoms");
    AtomRegistry::load_from_dir(&dir).unwrap()
}

fn live_archetypes() -> ArchetypeRegistry {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("config/archetypes");
    ArchetypeRegistry::load_from_dir(&dir).unwrap()
}

#[test]
fn compose_against_real_catalog_for_bulk_de() {
    // bulk_rnaseq_de archetype produces data:0951 (DE results)
    // in format:3475 (tabular text). The composer should match
    // it for a goal targeting that triple.
    let atom_reg = live_atoms();
    let arch_reg = live_archetypes();
    if atom_reg.is_empty() || arch_reg.is_empty() {
        return;
    }
    let goal = GoalSpec {
        edam_data: "data:0951".into(),
        edam_format: Some("format:3475".into()),
        modifiers: BTreeMap::new(),
        source_prose: None,
        confidence: 0.9,
    };
    // `compose()` was re-routed to v4;
    // this test exercises the legacy archetype-fast-path against
    // a real catalog and stays pinned at v2 explicitly.
    let result = compose_with_version(&goal, "bioinformatics", &atom_reg, &arch_reg, 2);
    // The exact archetype id depends on the catalog's scoring;
    // either bulk_rnaseq_de or single_cell_de may match. Assert
    // some archetype matched + atoms resolved.
    match result {
        Ok(r) => {
            assert!(!r.atoms.is_empty(), "composition produced zero atoms");
            assert!(r.match_score >= 3, "top score should be ≥ 3");
        }
        Err(CompositionError::TieRequiresSmeDecision { .. }) => {
            // Acceptable — multiple archetypes scored equally.
        }
        Err(e) => panic!("unexpected composition error: {}", e),
    }
}

#[test]
fn compose_returns_no_match_for_unknown_goal() {
    let atom_reg = live_atoms();
    let arch_reg = live_archetypes();
    if arch_reg.is_empty() {
        return;
    }
    let goal = GoalSpec {
        edam_data: "data:99999".into(),
        edam_format: None,
        modifiers: BTreeMap::new(),
        source_prose: None,
        confidence: 0.5,
    };
    // `compose()` re-routes to v4;
    // pin v2 here so the legacy `NoArchetypeMatch` shape stays
    // under test.
    let result = compose_with_version(&goal, "bogus_class", &atom_reg, &arch_reg, 2);
    assert!(matches!(
        result,
        Err(CompositionError::NoArchetypeMatch { .. })
    ));
}

#[test]
fn compose_is_deterministic_across_calls() {
    // Determinism: identical inputs yield byte-identical output.
    // Lock this in so a refactor that introduces a HashMap (vs
    // BTreeMap) or a clock read regresses the contract.
    let atom_reg = live_atoms();
    let arch_reg = live_archetypes();
    if atom_reg.is_empty() || arch_reg.is_empty() {
        return;
    }
    let goal = GoalSpec {
        edam_data: "data:0951".into(),
        edam_format: Some("format:3475".into()),
        modifiers: BTreeMap::new(),
        source_prose: None,
        confidence: 0.9,
    };
    let a = compose(&goal, "bioinformatics", &atom_reg, &arch_reg);
    let b = compose(&goal, "bioinformatics", &atom_reg, &arch_reg);
    assert_eq!(a, b);
}

/// 100× determinism replay test. Locks the
/// byte-deterministic-output contract so any future refactor that
/// introduces non-determinism (HashMap, clock reads, random ids,
/// rayon parallelism) regresses immediately.
#[test]
fn compose_is_byte_deterministic_across_100_replays() {
    let atom_reg = live_atoms();
    let arch_reg = live_archetypes();
    if atom_reg.is_empty() || arch_reg.is_empty() {
        return;
    }
    let goal = GoalSpec {
        edam_data: "data:0951".into(),
        edam_format: Some("format:3475".into()),
        modifiers: {
            let mut m = BTreeMap::new();
            m.insert("with_pathway_enrichment".into(), "true".into());
            m
        },
        source_prose: Some("compare DEGs across two arms".into()),
        confidence: 0.95,
    };
    let baseline = compose(&goal, "bioinformatics", &atom_reg, &arch_reg);
    for i in 0..100 {
        let replay = compose(&goal, "bioinformatics", &atom_reg, &arch_reg);
        assert_eq!(
            replay, baseline,
            "replay {} diverged from baseline; non-deterministic compose()",
            i
        );
    }
}

// ── Tests for the new validation arms (S7.4) ──────────────────────

use crate::atom::{AtomAssignee, AtomDefinition, AtomRole, MethodChoiceRef};

fn make_atom(
    id: &str,
    role: AtomRole,
    edam_data: Option<&str>,
    edam_format: Option<&str>,
    depends_on: Vec<&str>,
) -> AtomDefinition {
    AtomDefinition {
        id: id.into(),
        version: "1.0.0".into(),
        role,
        discovery_kind: if matches!(role, AtomRole::Discovery) {
            Some("method".into())
        } else {
            None
        },
        description: "test".into(),
        edam_operation: "operation:0004".into(),
        edam_data: edam_data.map(|s| s.into()),
        edam_format: edam_format.map(|s| s.into()),
        assignee: AtomAssignee::Agent,
        depends_on: depends_on.into_iter().map(|s| s.into()).collect(),
        excludes: vec![],
        attributes: BTreeMap::new(),
        joint_with: vec![],
        inputs: vec![],
        outputs: vec![],
        method_choice: None,
        resource_profile: None,
        preferred_container: None,
        claim_boundary: None,
        iterate: None,
        condition: None,
        required_figures: vec![],
        plot_stage_id: None,
        figure_exempt: None,
        expected_artifacts: vec![],
        required_artifacts: vec![],
        validators: vec![],
        runtime_packages: Default::default(),
        safety: Default::default(),
    }
}

fn composed_from(atom: AtomDefinition) -> ComposedAtom {
    let depends_on = atom.depends_on.clone();
    ComposedAtom {
        stage_id: atom.id.clone().into(),
        atom,
        depends_on,
        required: true,
        bindings: Vec::new(),
        container: None,
    }
}

#[test]
#[allow(deprecated)]
fn resolve_task_container_atom_override_wins() {
    // Atom-level pin beats archetype + profile defaults.
    let mut atom = make_atom("a", AtomRole::Operation, None, None, vec![]);
    atom.preferred_container = Some(ContainerSpec {
        image: "ghcr.io/scripps/scripps-bio-base".into(),
        tag: "1.0".into(),
        digest: String::new(),
        arch: vec!["amd64".into()],
        gpu_required: false,
        network: None,
        source: crate::atom::ContainerSource::default(),
    });
    let archetype_default = ContainerSpec {
        image: "ghcr.io/scripps/archetype-default".into(),
        tag: "1.0".into(),
        digest: String::new(),
        arch: vec!["amd64".into()],
        gpu_required: false,
        network: None,
        source: crate::atom::ContainerSource::default(),
    };
    let profile_default = ContainerSpec {
        image: "ghcr.io/scripps/profile-default".into(),
        tag: "1.0".into(),
        digest: String::new(),
        arch: vec!["amd64".into()],
        gpu_required: false,
        network: None,
        source: crate::atom::ContainerSource::default(),
    };
    let resolved = resolve_task_container(&atom, Some(&archetype_default), Some(&profile_default))
        .expect("must resolve to atom override");
    assert_eq!(resolved.image, "ghcr.io/scripps/scripps-bio-base");
}

#[test]
#[allow(deprecated)]
fn resolve_task_container_archetype_default_falls_through() {
    // Atom unset → archetype default wins over profile default.
    let atom = make_atom("a", AtomRole::Operation, None, None, vec![]);
    assert!(atom.preferred_container.is_none());
    let archetype_default = ContainerSpec {
        image: "ghcr.io/scripps/archetype-default".into(),
        tag: "1.0".into(),
        digest: String::new(),
        arch: vec!["amd64".into()],
        gpu_required: false,
        network: None,
        source: crate::atom::ContainerSource::default(),
    };
    let profile_default = ContainerSpec {
        image: "ghcr.io/scripps/profile-default".into(),
        tag: "1.0".into(),
        digest: String::new(),
        arch: vec!["amd64".into()],
        gpu_required: false,
        network: None,
        source: crate::atom::ContainerSource::default(),
    };
    let resolved = resolve_task_container(&atom, Some(&archetype_default), Some(&profile_default))
        .expect("must resolve to archetype default");
    assert_eq!(resolved.image, "ghcr.io/scripps/archetype-default");
}

#[test]
#[allow(deprecated)]
fn resolve_task_container_profile_default_lowest_precedence() {
    // Atom + archetype unset → profile default wins.
    let atom = make_atom("a", AtomRole::Operation, None, None, vec![]);
    let profile_default = ContainerSpec {
        image: "ghcr.io/scripps/profile-default".into(),
        tag: "1.0".into(),
        digest: String::new(),
        arch: vec!["amd64".into()],
        gpu_required: false,
        network: None,
        source: crate::atom::ContainerSource::default(),
    };
    let resolved =
        resolve_task_container(&atom, None, Some(&profile_default)).expect("profile default wins");
    assert_eq!(resolved.image, "ghcr.io/scripps/profile-default");
}

#[test]
fn resolve_task_container_returns_none_when_all_unset() {
    // Every level unset → host-mode (None).
    let atom = make_atom("a", AtomRole::Operation, None, None, vec![]);
    assert!(resolve_task_container(&atom, None, None).is_none());
}

#[test]
#[allow(deprecated)]
fn compose_threads_container_from_atom_into_composed_atom() {
    // End-to-end: an atom with preferred_container is composed
    // and its container shows up on `ComposedAtom::container`.
    // No archetype matches the goal, so the backward-chain path
    // runs and threads atom.preferred_container through.
    let mut atom = make_atom(
        "produce_x",
        AtomRole::Operation,
        Some("data:0951"),
        Some("format:3475"),
        vec![],
    );
    atom.preferred_container = Some(ContainerSpec {
        image: "ghcr.io/scripps/x-runner".into(),
        tag: "2.1".into(),
        digest: String::new(),
        arch: vec!["amd64".into()],
        gpu_required: false,
        network: None,
        source: crate::atom::ContainerSource::default(),
    });
    let atoms = registry_from(vec![atom]);
    let archetypes = empty_archetype_reg();
    let goal = GoalSpec {
        edam_data: "data:0951".into(),
        edam_format: Some("format:3475".into()),
        modifiers: BTreeMap::new(),
        source_prose: None,
        confidence: 0.9,
    };
    // Synthetic backward-chain test;
    // route through v2 (archetype empty → backward-chain
    // fallback) since v4 expects richer port contracts.
    let result = compose_with_version(&goal, "bioinformatics", &atoms, &archetypes, 2)
        .expect("backward-chain must succeed");
    assert_eq!(result.atoms.len(), 1);
    let container = result.atoms[0]
        .container
        .as_ref()
        .expect("container must thread through");
    assert_eq!(container.image, "ghcr.io/scripps/x-runner");
    assert_eq!(container.tag, "2.1");
}

fn empty_archetype_reg() -> ArchetypeRegistry {
    ArchetypeRegistry::default()
}

fn registry_from(atoms: Vec<AtomDefinition>) -> AtomRegistry {
    let tmp = tempfile::tempdir().unwrap();
    for atom in &atoms {
        let path = tmp.path().join(format!("{}.yaml", atom.id));
        let yaml = serde_yml::to_string(atom).unwrap();
        std::fs::write(&path, yaml).unwrap();
    }
    AtomRegistry::load_from_dir(tmp.path()).unwrap()
}

#[test]
fn detect_cycle_finds_simple_two_node_cycle() {
    // node_a depends_on node_b, node_b depends_on node_a — cycle.
    let a = composed_from(make_atom(
        "node_a",
        AtomRole::Operation,
        Some("data:0951"),
        Some("format:3475"),
        vec!["node_b"],
    ));
    let b = composed_from(make_atom(
        "node_b",
        AtomRole::Operation,
        Some("data:0951"),
        Some("format:3475"),
        vec!["node_a"],
    ));
    let atoms = vec![a, b];
    let composed_ids: BTreeSet<&str> = atoms.iter().map(|c| c.stage_id.as_str()).collect();
    let cycle = detect_cycle(&atoms, &composed_ids);
    assert!(cycle.is_some(), "expected to detect a 2-node cycle");
    let cycle = cycle.unwrap();
    assert!(cycle.contains(&"node_a".to_string()) && cycle.contains(&"node_b".to_string()));
}

#[test]
fn validate_rejects_cycle_via_compose_path() {
    let a = make_atom(
        "cycle_a",
        AtomRole::Operation,
        Some("data:0951"),
        Some("format:3475"),
        vec!["cycle_b"],
    );
    let b = make_atom(
        "cycle_b",
        AtomRole::Operation,
        Some("data:0951"),
        Some("format:3475"),
        vec!["cycle_a"],
    );
    let atom_reg = registry_from(vec![a, b]);
    let goal = GoalSpec {
        edam_data: "data:0951".into(),
        edam_format: Some("format:3475".into()),
        modifiers: BTreeMap::new(),
        source_prose: None,
        confidence: 0.9,
    };
    // Synthetic cycle detection test
    // pinned at v2; v4's planner has its own cycle handling.
    let result = compose_with_version(
        &goal,
        "bioinformatics",
        &atom_reg,
        &empty_archetype_reg(),
        2,
    );
    match result {
        Err(CompositionError::CycleDetected { .. }) => {}
        other => panic!("expected CycleDetected, got: {:?}", other),
    }
}

#[test]
fn validate_rejects_unreachable_goal() {
    let leaf = make_atom(
        "produces_other",
        AtomRole::Operation,
        Some("data:1383"),
        Some("format:1929"),
        vec![],
    );
    let result = CompositionResult {
        matched_archetype: Some("test_archetype".into()),
        match_score: 3,
        atoms: vec![composed_from(leaf.clone())],
        goal: GoalSpec {
            edam_data: "data:0951".into(),
            edam_format: Some("format:3475".into()),
            modifiers: BTreeMap::new(),
            source_prose: None,
            confidence: 0.9,
        },
        rationale: "test".into(),
        atom_rationales: BTreeMap::new(),
        resource_estimate: ResourceEstimate::default(),
    };
    let atom_reg = registry_from(vec![leaf]);
    let err = validate_composition(&result, &atom_reg);
    assert!(matches!(err, Err(CompositionError::GoalUnreachable { .. })));
}

#[test]
fn validate_accepts_subtype_match_for_goal() {
    let leaf = make_atom(
        "produces_goal",
        AtomRole::Operation,
        Some("data:0951"),
        Some("format:3475"),
        vec![],
    );
    let result = CompositionResult {
        matched_archetype: None,
        match_score: 0,
        atoms: vec![composed_from(leaf.clone())],
        goal: GoalSpec {
            edam_data: "data:0951".into(),
            edam_format: Some("format:3475".into()),
            modifiers: BTreeMap::new(),
            source_prose: None,
            confidence: 0.9,
        },
        rationale: "test".into(),
        atom_rationales: BTreeMap::new(),
        resource_estimate: ResourceEstimate::default(),
    };
    let atom_reg = registry_from(vec![leaf]);
    validate_composition(&result, &atom_reg).expect("goal-reachable validation should pass");
}

#[test]
fn validate_rejects_input_unsatisfied() {
    let producer = make_atom(
        "producer",
        AtomRole::Operation,
        Some("data:1383"),
        Some("format:1929"),
        vec![],
    );
    let consumer = make_atom(
        "consumer",
        AtomRole::Operation,
        Some("data:0951"),
        Some("format:3475"),
        vec!["producer"],
    );
    let atom_reg = registry_from(vec![producer.clone(), consumer.clone()]);
    let result = CompositionResult {
        matched_archetype: Some("partial".into()),
        match_score: 3,
        atoms: vec![composed_from(consumer)],
        goal: GoalSpec {
            edam_data: "data:0951".into(),
            edam_format: Some("format:3475".into()),
            modifiers: BTreeMap::new(),
            source_prose: None,
            confidence: 0.9,
        },
        rationale: "test".into(),
        atom_rationales: BTreeMap::new(),
        resource_estimate: ResourceEstimate::default(),
    };
    let err = validate_composition(&result, &atom_reg);
    assert!(matches!(
        err,
        Err(CompositionError::InputUnsatisfied { .. })
    ));
}

#[test]
fn validate_accepts_intake_supplied_dep() {
    let consumer = make_atom(
        "consumer",
        AtomRole::Operation,
        Some("data:0951"),
        Some("format:3475"),
        vec!["intake_field"],
    );
    let atom_reg = registry_from(vec![consumer.clone()]);
    let result = CompositionResult {
        matched_archetype: None,
        match_score: 0,
        atoms: vec![composed_from(consumer)],
        goal: GoalSpec {
            edam_data: "data:0951".into(),
            edam_format: Some("format:3475".into()),
            modifiers: BTreeMap::new(),
            source_prose: None,
            confidence: 0.9,
        },
        rationale: "test".into(),
        atom_rationales: BTreeMap::new(),
        resource_estimate: ResourceEstimate::default(),
    };
    validate_composition(&result, &atom_reg).expect("intake dep should pass validation");
}

#[test]
fn validate_rejects_method_choice_unresolved() {
    let mut op = make_atom(
        "use_method",
        AtomRole::Operation,
        Some("data:0951"),
        Some("format:3475"),
        vec![],
    );
    op.method_choice = Some(MethodChoiceRef {
        deferred_to: "discover_aligner".into(),
    });
    let discover = make_atom("discover_aligner", AtomRole::Discovery, None, None, vec![]);
    let atom_reg = registry_from(vec![op.clone(), discover]);
    let result = CompositionResult {
        matched_archetype: None,
        match_score: 0,
        atoms: vec![composed_from(op)],
        goal: GoalSpec {
            edam_data: "data:0951".into(),
            edam_format: Some("format:3475".into()),
            modifiers: BTreeMap::new(),
            source_prose: None,
            confidence: 0.9,
        },
        rationale: "test".into(),
        atom_rationales: BTreeMap::new(),
        resource_estimate: ResourceEstimate::default(),
    };
    let err = validate_composition(&result, &atom_reg);
    assert!(matches!(
        err,
        Err(CompositionError::MethodChoiceUnresolved { .. })
    ));
}

#[test]
fn validate_accepts_method_choice_when_discovery_present() {
    let mut op = make_atom(
        "use_method",
        AtomRole::Operation,
        Some("data:0951"),
        Some("format:3475"),
        vec![],
    );
    op.method_choice = Some(MethodChoiceRef {
        deferred_to: "discover_aligner".into(),
    });
    let discover = make_atom("discover_aligner", AtomRole::Discovery, None, None, vec![]);
    let atom_reg = registry_from(vec![op.clone(), discover.clone()]);
    let result = CompositionResult {
        matched_archetype: None,
        match_score: 0,
        atoms: vec![composed_from(discover), composed_from(op)],
        goal: GoalSpec {
            edam_data: "data:0951".into(),
            edam_format: Some("format:3475".into()),
            modifiers: BTreeMap::new(),
            source_prose: None,
            confidence: 0.9,
        },
        rationale: "test".into(),
        atom_rationales: BTreeMap::new(),
        resource_estimate: ResourceEstimate::default(),
    };
    validate_composition(&result, &atom_reg)
        .expect("composition with discovery sibling should pass");
}

#[test]
fn validate_rejects_malformed_exclusion() {
    let mut atom = make_atom(
        "tool_solo",
        AtomRole::Operation,
        Some("data:0951"),
        Some("format:3475"),
        vec![],
    );
    atom.excludes = vec!["ghost_atom".into()];
    let atom_reg = registry_from(vec![atom.clone()]);
    let result = CompositionResult {
        matched_archetype: None,
        match_score: 0,
        atoms: vec![composed_from(atom)],
        goal: GoalSpec {
            edam_data: "data:0951".into(),
            edam_format: Some("format:3475".into()),
            modifiers: BTreeMap::new(),
            source_prose: None,
            confidence: 0.9,
        },
        rationale: "test".into(),
        atom_rationales: BTreeMap::new(),
        resource_estimate: ResourceEstimate::default(),
    };
    let err = validate_composition(&result, &atom_reg);
    assert!(matches!(
        err,
        Err(CompositionError::MalformedExclusion { .. })
    ));
}

#[test]
fn validate_rejects_exclusion_conflict() {
    let mut a = make_atom(
        "tool_a",
        AtomRole::Operation,
        Some("data:0951"),
        Some("format:3475"),
        vec![],
    );
    a.excludes = vec!["tool_b".into()];
    let b = make_atom(
        "tool_b",
        AtomRole::Operation,
        Some("data:0951"),
        Some("format:3475"),
        vec![],
    );
    let atom_reg = registry_from(vec![a.clone(), b.clone()]);
    let result = CompositionResult {
        matched_archetype: Some("dual".into()),
        match_score: 3,
        atoms: vec![composed_from(a), composed_from(b)],
        goal: GoalSpec {
            edam_data: "data:0951".into(),
            edam_format: Some("format:3475".into()),
            modifiers: BTreeMap::new(),
            source_prose: None,
            confidence: 0.9,
        },
        rationale: "test".into(),
        atom_rationales: BTreeMap::new(),
        resource_estimate: ResourceEstimate::default(),
    };
    let err = validate_composition(&result, &atom_reg);
    assert!(matches!(
        err,
        Err(CompositionError::ExclusionConflict { .. })
    ));
}

// ── Backward-chain composer (S7.2) tests ──────────────────────────

#[test]
fn backward_chain_composes_single_producer() {
    let leaf = make_atom(
        "make_de",
        AtomRole::Operation,
        Some("data:0951"),
        Some("format:3475"),
        vec![],
    );
    let atom_reg = registry_from(vec![leaf]);
    let goal = GoalSpec {
        edam_data: "data:0951".into(),
        edam_format: Some("format:3475".into()),
        modifiers: BTreeMap::new(),
        source_prose: None,
        confidence: 0.9,
    };
    // Synthetic backward-chain test;
    // route through v2 (no archetype → backward-chain fallback).
    let result = compose_with_version(
        &goal,
        "bioinformatics",
        &atom_reg,
        &empty_archetype_reg(),
        2,
    )
    .unwrap();
    assert!(result.matched_archetype.is_none());
    assert_eq!(result.atoms.len(), 1);
    assert_eq!(result.atoms[0].stage_id.as_str(), "make_de");
}

#[test]
fn backward_chain_walks_dependency_chain() {
    let prep = make_atom(
        "prep",
        AtomRole::Operation,
        Some("data:1383"),
        Some("format:1929"),
        vec![],
    );
    let mid = make_atom(
        "mid",
        AtomRole::Operation,
        Some("data:3917"),
        Some("format:3590"),
        vec!["prep"],
    );
    let leaf = make_atom(
        "leaf",
        AtomRole::Operation,
        Some("data:0951"),
        Some("format:3475"),
        vec!["mid"],
    );
    let atom_reg = registry_from(vec![prep, mid, leaf]);
    let goal = GoalSpec {
        edam_data: "data:0951".into(),
        edam_format: Some("format:3475".into()),
        modifiers: BTreeMap::new(),
        source_prose: None,
        confidence: 0.9,
    };
    // Synthetic backward-chain test
    // pinned at v2.
    let result = compose_with_version(
        &goal,
        "bioinformatics",
        &atom_reg,
        &empty_archetype_reg(),
        2,
    )
    .unwrap();
    assert!(result.matched_archetype.is_none());
    let ids: Vec<&str> = result.atoms.iter().map(|c| c.stage_id.as_str()).collect();
    assert_eq!(ids, vec!["prep", "mid", "leaf"]);
}

#[test]
fn backward_chain_prunes_to_shortest_chain() {
    let direct = make_atom(
        "a_direct",
        AtomRole::Operation,
        Some("data:0951"),
        Some("format:3475"),
        vec![],
    );
    let via_mid_dep = make_atom(
        "z_dep",
        AtomRole::Operation,
        Some("data:1383"),
        Some("format:1929"),
        vec![],
    );
    let via_mid = make_atom(
        "z_via_mid",
        AtomRole::Operation,
        Some("data:0951"),
        Some("format:3475"),
        vec!["z_dep"],
    );
    let atom_reg = registry_from(vec![direct, via_mid_dep, via_mid]);
    let goal = GoalSpec {
        edam_data: "data:0951".into(),
        edam_format: Some("format:3475".into()),
        modifiers: BTreeMap::new(),
        source_prose: None,
        confidence: 0.9,
    };
    // Synthetic backward-chain test
    // pinned at v2.
    let result = compose_with_version(
        &goal,
        "bioinformatics",
        &atom_reg,
        &empty_archetype_reg(),
        2,
    )
    .unwrap();
    assert_eq!(result.atoms.len(), 1);
    assert_eq!(result.atoms[0].stage_id.as_str(), "a_direct");
}

#[test]
fn backward_chain_returns_no_match_when_registry_empty() {
    let atom_reg = registry_from(vec![]);
    let goal = GoalSpec {
        edam_data: "data:0951".into(),
        edam_format: None,
        modifiers: BTreeMap::new(),
        source_prose: None,
        confidence: 0.9,
    };
    // Synthetic backward-chain
    // empty-registry test pinned at v2.
    let result = compose_with_version(
        &goal,
        "bioinformatics",
        &atom_reg,
        &empty_archetype_reg(),
        2,
    );
    assert!(matches!(
        result,
        Err(CompositionError::NoArchetypeMatch { .. })
    ));
}

// ── slot-fill tests ─────────────────────────────────

/// Build an in-memory `PortMappingRegistry` from a literal YAML
/// blob. Mirrors the on-disk shape `PortMappingRegistry::load`
/// reads so the test stays close to the production wiring.
fn port_mappings_from(yaml: &str) -> PortMappingRegistry {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("intake-port-mapping.yaml");
    std::fs::write(&path, yaml).unwrap();
    PortMappingRegistry::load(&path).unwrap()
}

#[test]
fn slot_fill_all_slots_filled_cleanly() {
    // (a) When every atom's primary input + depends_on chain is
    // either filled by an upstream composed atom or by an SME-
    // supplied intake field, slot-fill emits one binding per slot
    // and the composition stays intact.
    let producer = make_atom(
        "data_acquisition",
        AtomRole::Operation,
        Some("data:2531"),
        Some("format:1930"),
        vec![],
    );
    let consumer = make_atom(
        "differential_expression",
        AtomRole::Operation,
        Some("data:0951"),
        Some("format:3475"),
        vec!["data_acquisition"],
    );

    let mappings = port_mappings_from(
        r#"version: 1
rules:
  - intake_field: samples
    edam_data: data:2531
    edam_format: format:1930
    cardinality: collection
"#,
    );
    let intake = IntakeContext::new(["samples".to_string()], &mappings);

    // Construct the composition by hand so the test exercises the
    // slot-fill pass directly against a known atom shape; the
    // public `compose_with_intake` entry point routes through the
    // archetype matcher, which adds catalog dependence the
    // slot-fill test doesn't need.
    let mut result = CompositionResult {
        matched_archetype: Some("test_archetype".into()),
        match_score: 6,
        atoms: vec![
            ComposedAtom {
                stage_id: "data_acquisition".into(),
                atom: producer.clone(),
                depends_on: vec![],
                required: true,
                bindings: Vec::new(),
                container: None,
            },
            ComposedAtom {
                stage_id: "differential_expression".into(),
                atom: consumer.clone(),
                depends_on: vec!["data_acquisition".into()],
                required: true,
                bindings: Vec::new(),
                container: None,
            },
        ],
        goal: GoalSpec {
            edam_data: "data:0951".into(),
            edam_format: Some("format:3475".into()),
            modifiers: BTreeMap::new(),
            source_prose: None,
            confidence: 0.9,
        },
        rationale: "test".into(),
        atom_rationales: BTreeMap::new(),
        resource_estimate: ResourceEstimate::default(),
    };
    apply_slot_fill(&mut result, &intake).expect("clean slot-fill should succeed");

    assert_eq!(result.atoms.len(), 2);
    // The producer's primary input is data:2531 — the SME's
    // `samples` intake field supplies that EDAM data class.
    let producer_atom = result
        .atoms
        .iter()
        .find(|c| c.stage_id.as_str() == "data_acquisition")
        .expect("producer kept");
    assert!(
        producer_atom
            .bindings
            .iter()
            .any(|b| b.slot == "primary_input"
                && matches!(&b.source, SlotSource::IntakeField(f) if f == "samples")),
        "producer should bind primary_input to samples intake field; got {:?}",
        producer_atom.bindings,
    );
    // The consumer's depends_on slot for `data_acquisition` is
    // filled by the upstream atom (the load-bearing edge).
    let consumer_atom = result
        .atoms
        .iter()
        .find(|c| c.stage_id.as_str() == "differential_expression")
        .expect("consumer kept");
    assert!(
        consumer_atom
            .bindings
            .iter()
            .any(|b| b.slot == "data_acquisition"
                && matches!(
                    &b.source,
                    SlotSource::UpstreamAtom(s) if s == "data_acquisition"
                )),
        "consumer should bind dep slot to upstream atom; got {:?}",
        consumer_atom.bindings,
    );
}

#[test]
fn slot_fill_drops_optional_atom_with_unfilled_slot() {
    // (b) An optional atom whose primary input has no upstream
    // producer AND no intake-supplied field is silently dropped
    // from the composition, and the dropped atom's id is pruned
    // from any surviving atom's depends_on edges.
    let producer = make_atom(
        "data_acquisition",
        AtomRole::Operation,
        Some("data:2531"),
        Some("format:1930"),
        vec![],
    );
    // Optional atom expecting data:1383 (Sequence assembly
    // reference) — neither produced upstream nor supplied via
    // intake.
    let optional = make_atom(
        "optional_reference",
        AtomRole::Operation,
        Some("data:1383"),
        Some("format:1929"),
        vec![],
    );
    let consumer = make_atom(
        "consumer",
        AtomRole::Operation,
        Some("data:0951"),
        Some("format:3475"),
        vec!["data_acquisition", "optional_reference"],
    );

    let mappings = port_mappings_from(
        r#"version: 1
rules:
  - intake_field: samples
    edam_data: data:2531
    edam_format: format:1930
    cardinality: collection
"#,
    );

    let mut result = CompositionResult {
        matched_archetype: Some("test_archetype".into()),
        match_score: 3,
        atoms: vec![
            ComposedAtom {
                stage_id: "data_acquisition".into(),
                atom: producer.clone(),
                depends_on: vec![],
                required: true,
                bindings: Vec::new(),
                container: None,
            },
            ComposedAtom {
                stage_id: "optional_reference".into(),
                atom: optional.clone(),
                depends_on: vec![],
                required: false,
                bindings: Vec::new(),
                container: None,
            },
            ComposedAtom {
                stage_id: "consumer".into(),
                atom: consumer.clone(),
                depends_on: vec!["data_acquisition".into(), "optional_reference".into()],
                required: true,
                bindings: Vec::new(),
                container: None,
            },
        ],
        goal: GoalSpec {
            edam_data: "data:0951".into(),
            edam_format: Some("format:3475".into()),
            modifiers: BTreeMap::new(),
            source_prose: None,
            confidence: 0.9,
        },
        rationale: "test".into(),
        atom_rationales: BTreeMap::new(),
        resource_estimate: ResourceEstimate::default(),
    };
    let intake = IntakeContext::new(["samples".to_string()], &mappings);
    apply_slot_fill(&mut result, &intake).expect("optional drop should not error");

    // optional_reference is gone; consumer is still present but
    // its depends_on no longer references the dropped atom.
    let ids: Vec<&str> = result.atoms.iter().map(|c| c.stage_id.as_str()).collect();
    assert_eq!(ids, vec!["data_acquisition", "consumer"]);
    let consumer = result
        .atoms
        .iter()
        .find(|c| c.stage_id.as_str() == "consumer")
        .unwrap();
    assert_eq!(consumer.depends_on, vec!["data_acquisition".to_string()]);
}

#[test]
fn slot_fill_required_unfilled_returns_typed_error() {
    // (c) A required atom whose primary input has no upstream
    // producer AND no intake-supplied field returns
    // `CompositionError::UnfilledRequiredSlot { atom, slot,
    // expected }`. The atom's expected EDAM type is data:1383
    // (Sequence assembly), and the port-mapping registry only
    // declares data:2531 (samples) — even with samples supplied,
    // no rule satisfies the slot's expected type.
    let consumer = make_atom(
        "needs_reference",
        AtomRole::Operation,
        Some("data:1383"),
        Some("format:1929"),
        vec![],
    );
    let mappings = port_mappings_from(
        r#"version: 1
rules:
  - intake_field: samples
    edam_data: data:2531
    edam_format: format:1930
    cardinality: collection
"#,
    );
    let intake = IntakeContext::new(["samples".to_string()], &mappings);

    let mut result = CompositionResult {
        matched_archetype: Some("test_archetype".into()),
        match_score: 3,
        atoms: vec![ComposedAtom {
            stage_id: "needs_reference".into(),
            atom: consumer.clone(),
            depends_on: vec![],
            required: true,
            bindings: Vec::new(),
            container: None,
        }],
        goal: GoalSpec {
            edam_data: "data:1383".into(),
            edam_format: Some("format:1929".into()),
            modifiers: BTreeMap::new(),
            source_prose: None,
            confidence: 0.9,
        },
        rationale: "test".into(),
        atom_rationales: BTreeMap::new(),
        resource_estimate: ResourceEstimate::default(),
    };
    let err = apply_slot_fill(&mut result, &intake)
        .expect_err("required unfilled slot must surface typed error");
    match err {
        CompositionError::UnfilledRequiredSlot {
            atom,
            slot,
            expected,
        } => {
            assert_eq!(atom, "needs_reference");
            assert_eq!(slot, "primary_input");
            assert_eq!(expected, "data:1383");
        }
        other => panic!("expected UnfilledRequiredSlot, got: {:?}", other),
    }
}

// `compose_with_version_3_forces_backward_chain` test was
// retired alongside the v3 entry point. v3 now routes through
// v2 (archetype fast-path with backward-chain fallback when no
// archetype matches), which is observable through the surviving
// `compose_with_version_unknown_falls_back_to_v2` test below.

/// Joint-source constraint passes when both
/// producers carry matching `attributes.source_atom`.
#[test]
fn joint_with_validates_when_sources_match() {
    use crate::atom::JointlyWithConstraint;

    let mut counts = make_atom(
        "counts",
        AtomRole::Operation,
        Some("data:3917"),
        Some("format:3590"),
        vec![],
    );
    counts
        .attributes
        .insert("source_atom".into(), serde_json::json!("sample_a"));
    let mut protein = make_atom(
        "protein",
        AtomRole::Operation,
        Some("data:3917"),
        Some("format:3590"),
        vec![],
    );
    protein
        .attributes
        .insert("source_atom".into(), serde_json::json!("sample_a"));
    let mut integrate = make_atom(
        "integrate",
        AtomRole::Operation,
        Some("data:3917"),
        Some("format:3590"),
        vec!["counts", "protein"],
    );
    integrate.joint_with = vec![JointlyWithConstraint {
        lhs: "counts".into(),
        rhs: "protein".into(),
    }];

    let result = CompositionResult {
        matched_archetype: Some("multi_modal_integration".into()),
        match_score: 5,
        atoms: vec![
            composed_from(counts.clone()),
            composed_from(protein.clone()),
            composed_from(integrate.clone()),
        ],
        goal: GoalSpec {
            edam_data: "data:3917".into(),
            edam_format: Some("format:3590".into()),
            modifiers: BTreeMap::new(),
            source_prose: None,
            confidence: 0.0,
        },
        rationale: "ok".into(),
        atom_rationales: BTreeMap::new(),
        resource_estimate: ResourceEstimate::default(),
    };
    let registry = registry_from(vec![counts, protein, integrate]);
    // Six-item validation passes (joint source ok).
    validate_composition(&result, &registry).expect("joint validation passes");
}

/// Joint-source constraint fires
/// `JointSourceMismatch` when producers carry diverging
/// `attributes.source_atom`.
#[test]
fn joint_with_rejects_mismatched_sources() {
    use crate::atom::JointlyWithConstraint;

    let mut counts = make_atom(
        "counts",
        AtomRole::Operation,
        Some("data:3917"),
        Some("format:3590"),
        vec![],
    );
    counts
        .attributes
        .insert("source_atom".into(), serde_json::json!("sample_a"));
    let mut protein = make_atom(
        "protein",
        AtomRole::Operation,
        Some("data:3917"),
        Some("format:3590"),
        vec![],
    );
    protein.attributes.insert(
        "source_atom".into(),
        serde_json::json!("sample_b"), // diverges!
    );
    let mut integrate = make_atom(
        "integrate",
        AtomRole::Operation,
        Some("data:3917"),
        Some("format:3590"),
        vec!["counts", "protein"],
    );
    integrate.joint_with = vec![JointlyWithConstraint {
        lhs: "counts".into(),
        rhs: "protein".into(),
    }];

    let result = CompositionResult {
        matched_archetype: Some("multi_modal_integration".into()),
        match_score: 5,
        atoms: vec![
            composed_from(counts.clone()),
            composed_from(protein.clone()),
            composed_from(integrate.clone()),
        ],
        goal: GoalSpec {
            edam_data: "data:3917".into(),
            edam_format: Some("format:3590".into()),
            modifiers: BTreeMap::new(),
            source_prose: None,
            confidence: 0.0,
        },
        rationale: "ok".into(),
        atom_rationales: BTreeMap::new(),
        resource_estimate: ResourceEstimate::default(),
    };
    let registry = registry_from(vec![counts, protein, integrate]);
    let err =
        validate_composition(&result, &registry).expect_err("must surface JointSourceMismatch");
    match err {
        CompositionError::JointSourceMismatch {
            atom,
            lhs_source,
            rhs_source,
            ..
        } => {
            assert_eq!(atom, "integrate");
            assert_eq!(lhs_source.as_deref(), Some("sample_a"));
            assert_eq!(rhs_source.as_deref(), Some("sample_b"));
        }
        other => panic!("expected JointSourceMismatch, got: {:?}", other),
    }
}

/// Missing `source_atom` attribute on a producer
/// is a constraint violation. The constraint requires both
/// sides to declare a source — silent omission is not a pass.
#[test]
fn joint_with_rejects_missing_source_attribute() {
    use crate::atom::JointlyWithConstraint;

    let counts = make_atom(
        "counts",
        AtomRole::Operation,
        Some("data:3917"),
        Some("format:3590"),
        vec![],
    );
    // counts has NO source_atom attribute.
    let mut protein = make_atom(
        "protein",
        AtomRole::Operation,
        Some("data:3917"),
        Some("format:3590"),
        vec![],
    );
    protein
        .attributes
        .insert("source_atom".into(), serde_json::json!("sample_a"));
    let mut integrate = make_atom(
        "integrate",
        AtomRole::Operation,
        Some("data:3917"),
        Some("format:3590"),
        vec!["counts", "protein"],
    );
    integrate.joint_with = vec![JointlyWithConstraint {
        lhs: "counts".into(),
        rhs: "protein".into(),
    }];

    let result = CompositionResult {
        matched_archetype: Some("multi_modal_integration".into()),
        match_score: 5,
        atoms: vec![
            composed_from(counts.clone()),
            composed_from(protein.clone()),
            composed_from(integrate.clone()),
        ],
        goal: GoalSpec {
            edam_data: "data:3917".into(),
            edam_format: Some("format:3590".into()),
            modifiers: BTreeMap::new(),
            source_prose: None,
            confidence: 0.0,
        },
        rationale: "ok".into(),
        atom_rationales: BTreeMap::new(),
        resource_estimate: ResourceEstimate::default(),
    };
    let registry = registry_from(vec![counts, protein, integrate]);
    let err = validate_composition(&result, &registry)
        .expect_err("must reject when one side lacks source_atom");
    assert!(matches!(err, CompositionError::JointSourceMismatch { .. }));
}

/// `composer_version` outside the {1, 2, 3} set
/// falls back to the v2 default routing (archetype fast-path).
///
/// The caller's modality / project_class / available_data is
/// threaded into the PlanningContext so the v4 planner can walk
/// the registry from a data:3498 (VCF) goal. The role-rank edge
/// filter on meet_in_middle keeps the variant-calling chain
/// acyclic so the dispatch succeeds.
///
/// The test now asserts: v4 dispatch either succeeds or returns
/// a structured `ComposerV4OutcomeNotExecutable` / Task-A-only
/// failure — what it actually guards against is the v1/v2/v3
/// archetype path being silently selected for a v4 dispatch
/// (which would surface as `NoArchetypeMatch`).
#[test]
fn compose_with_version_4_routes_to_v4_planner() {
    let config = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("config");
    let atoms = AtomRegistry::load_from_dir(&config.join("stage-atoms")).expect("load atoms");
    let archetypes =
        ArchetypeRegistry::load_from_dir(&config.join("archetypes")).expect("load archs");
    let goal = GoalSpec {
        edam_data: "data:3498".into(),
        edam_format: Some("format:3016".into()),
        modifiers: BTreeMap::new(),
        source_prose: Some("Test v4 dispatch".into()),
        confidence: 0.0,
    };
    let result = compose_with_version(&goal, "bioinformatics", &atoms, &archetypes, 4);
    match result {
        Ok(composition) => {
            // Verify the composition reached the canonical variant
            // chain (proves the v4 planner engaged — the archetype
            // fast-path wouldn't pull these atoms in because no
            // archetype matches data:3498 today).
            let stage_ids: BTreeSet<&str> = composition
                .atoms
                .iter()
                .map(|c| c.stage_id.as_str())
                .collect();
            assert!(
                stage_ids.iter().any(|id| id.contains("variant")),
                "v4 dispatch must reach variant-* atoms (proves planner engaged); \
                 got {stage_ids:?}"
            );
        }
        Err(CompositionError::ComposerV4OutcomeNotExecutable { outcome_kind, .. }) => {
            assert!(
                matches!(outcome_kind.as_str(), "PartialDag" | "DraftDag"),
                "v4 dispatch must surface PartialDag or DraftDag, got: {outcome_kind}"
            );
        }
        Err(CompositionError::CycleDetected { cycle }) => {
            panic!("Phase 4.5 Task B regressed — v4 dispatch hit a cycle; cycle={cycle:?}");
        }
        Err(other) => panic!(
            "v4 dispatch must surface Ok or ComposerV4OutcomeNotExecutable; \
             got {other:?}"
        ),
    }
}

#[test]
fn composition_error_no_archetype_match_maps_to_partial_dag() {
    // Typed
    // outcome wrapper. NoArchetypeMatch -> PartialDag with a
    // gap report.
    use crate::workflow_contracts::outcome::ComposeOutcome;
    let err = CompositionError::NoArchetypeMatch {
        target_data: "data:9999".into(),
        target_format: None,
        target_class: "research".into(),
    };
    let outcome = err.to_compose_outcome();
    match outcome {
        ComposeOutcome::PartialDag {
            unresolved_gaps, ..
        } => {
            assert_eq!(unresolved_gaps.len(), 1);
            assert_eq!(unresolved_gaps[0].id, "no_archetype_match");
            assert!(unresolved_gaps[0].statement.contains("data:9999"));
        }
        other => panic!("expected PartialDag, got {other:?}"),
    }
}

#[test]
fn composition_error_tie_maps_to_refusal() {
    use crate::workflow_contracts::outcome::ComposeOutcome;
    let err = CompositionError::TieRequiresSmeDecision {
        candidates: vec!["arch_a".into(), "arch_b".into()],
        score: 8,
    };
    let outcome = err.to_compose_outcome();
    match outcome {
        ComposeOutcome::Refusal { report } => {
            use crate::workflow_contracts::refusal_kind::RefusalKind;
            assert_eq!(report.id, "tie_requires_sme_decision");
            // v4 P4 / F21 — `kind` is now typed `RefusalKind`.
            // Ambiguous archetype tie maps to `GoalUnderspecified`
            // so the SME's recovery is to refine the goal.
            assert!(matches!(report.kind, RefusalKind::GoalUnderspecified));
            assert!(report.statement.contains("score 8"));
            // F21 invariant — non-hard kind must carry at least one path.
            assert!(!report.unblock_paths.is_empty());
            assert!(report.validate().is_ok());
        }
        other => panic!("expected Refusal, got {other:?}"),
    }
}

#[test]
fn composition_error_cycle_maps_to_refusal() {
    use crate::workflow_contracts::outcome::ComposeOutcome;
    use crate::workflow_contracts::refusal_kind::RefusalKind;
    let err = CompositionError::CycleDetected {
        cycle: vec!["a".into(), "b".into(), "a".into()],
    };
    let outcome = err.to_compose_outcome();
    match outcome {
        ComposeOutcome::Refusal { report } => {
            // v4 P4 / F21 — graph-shape errors are uncategorized
            // infrastructure refusals; the SME's recovery is to
            // escalate to a bioinformatics lead.
            assert!(matches!(report.kind, RefusalKind::UncategorizedBlocker));
            assert!(report.statement.contains("a -> b -> a"));
            assert!(!report.unblock_paths.is_empty());
            assert!(report.validate().is_ok());
        }
        other => panic!("expected Refusal, got {other:?}"),
    }
}

#[test]
fn composition_error_unfilled_slot_maps_to_partial_with_missing_port() {
    use crate::workflow_contracts::outcome::ComposeOutcome;
    let err = CompositionError::UnfilledRequiredSlot {
        atom: "quantify".into(),
        slot: "annotation".into(),
        expected: "data:1234".into(),
    };
    let outcome = err.to_compose_outcome();
    match outcome {
        ComposeOutcome::PartialDag {
            unresolved_gaps, ..
        } => {
            assert_eq!(
                unresolved_gaps[0].missing_port.as_deref(),
                Some("data:1234")
            );
            assert!(unresolved_gaps[0].id.contains("unfilled_slot"));
        }
        other => panic!("expected PartialDag, got {other:?}"),
    }
}

#[test]
fn composition_error_v4_partial_dag_maps_to_partial_with_alternatives_hint() {
    use crate::workflow_contracts::outcome::ComposeOutcome;
    let err = CompositionError::ComposerV4OutcomeNotExecutable {
        outcome_kind: "PartialDag".into(),
        summary: "v4 partial".into(),
        gaps: vec!["forward search not implemented".into()],
    };
    let outcome = err.to_compose_outcome();
    match outcome {
        ComposeOutcome::PartialDag {
            unresolved_gaps, ..
        } => {
            assert_eq!(unresolved_gaps.len(), 1);
            assert!(unresolved_gaps[0]
                .statement
                .contains("forward search not implemented"));
            assert!(unresolved_gaps[0]
                .suggestions
                .iter()
                .any(|s| s.contains("ranked_alternatives")));
        }
        other => panic!("expected PartialDag, got {other:?}"),
    }
}

#[test]
fn composition_error_v4_refusal_maps_to_refusal() {
    use crate::workflow_contracts::outcome::ComposeOutcome;
    use crate::workflow_contracts::refusal_kind::RefusalKind;
    let err = CompositionError::ComposerV4OutcomeNotExecutable {
        outcome_kind: "Refusal".into(),
        summary: "policy gate failed".into(),
        gaps: vec!["clinical_v3".into()],
    };
    let outcome = err.to_compose_outcome();
    match outcome {
        ComposeOutcome::Refusal { report } => {
            // The v4 planner refusal that comes through the
            // CompositionError path carries `UncategorizedBlocker`
            // (the richer category-aware refusal kind is set by
            // `composer_v4::planner` itself; the CompositionError
            // adapter is the back-compat path).
            assert!(matches!(report.kind, RefusalKind::UncategorizedBlocker));
            assert!(report.statement.contains("policy gate failed"));
            assert!(!report.unblock_paths.is_empty());
            assert!(report.validate().is_ok());
        }
        other => panic!("expected Refusal, got {other:?}"),
    }
}

#[test]
fn compose_with_version_unknown_falls_back_to_v2() {
    let config = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("config");
    let atoms = AtomRegistry::load_from_dir(&config.join("stage-atoms")).expect("load atoms");
    let archetypes =
        ArchetypeRegistry::load_from_dir(&config.join("archetypes")).expect("load archs");

    let goal = GoalSpec {
        edam_data: "data:3498".into(),
        edam_format: Some("format:3016".into()),
        modifiers: BTreeMap::new(),
        source_prose: None,
        confidence: 0.0,
    };

    // Unknown version (e.g. 99 from a future/forked deploy)
    // routes through v2 default — archetype matches.
    let r = compose_with_version(&goal, "bioinformatics", &atoms, &archetypes, 99)
        .expect("unknown version compose succeeds");
    assert!(
        r.matched_archetype.is_some(),
        "unknown version must fall back to v2 archetype routing"
    );
}

/// Iterate-until atoms compose as single nodes.
/// The composer ranges over `archetype.atoms` (archetype path)
/// or backward-chains through `depends_on` (backward-chain
/// path). Neither expands the runtime iteration chain — that
/// happens in the agent at dispatch time per §S10.4. This test
/// pins the contract: an atom carrying `iterate: Some(...)`
/// shows up exactly once in `CompositionResult.atoms` regardless
/// of `max_iterations`. The post-§S10.4 runtime expansion adds
/// `<id>_iter_N` tasks to the *DAG* (via the builder), not to
/// the composer's atom set.
#[test]
fn iterate_until_atom_composes_as_single_node() {
    use crate::atom::{IterateConvergence, IterateConvergenceOp, IterateMaxAction, IterateSpec};
    let mut producer = make_atom(
        "iterative_clustering",
        AtomRole::Operation,
        Some("data:0951"),
        Some("format:3475"),
        vec![],
    );
    producer.iterate = Some(IterateSpec {
        max_iterations: 25,
        min_iterations: 3,
        convergence: IterateConvergence {
            metric_source: "result.silhouette".into(),
            operator: IterateConvergenceOp::Gt,
            threshold: 0.6,
            consecutive_iterations: 2,
        },
        on_max_iterations: IterateMaxAction::Block,
        best_selector: Some("result.silhouette".into()),
    });
    let atom_reg = registry_from(vec![producer]);
    let goal = GoalSpec {
        edam_data: "data:0951".into(),
        edam_format: Some("format:3475".into()),
        modifiers: BTreeMap::new(),
        source_prose: None,
        confidence: 0.9,
    };
    // Synthetic iterate test pinned at
    // v2; v4's planner doesn't yet preserve the IterateSpec on
    // composed atoms produced from minimal synthetic registries.
    let result = compose_with_version(
        &goal,
        "bioinformatics",
        &atom_reg,
        &empty_archetype_reg(),
        2,
    )
    .expect("iterate-until atom should compose");
    // Composer treats iterate atoms exactly like any other —
    // one slot, one entry, no runtime expansion. The runtime
    // chain materialises later via the builder + agent.
    assert_eq!(
        result.atoms.len(),
        1,
        "iterate-until atom must compose as a single node, not max_iterations × N"
    );
    assert_eq!(result.atoms[0].atom.id, "iterative_clustering");
    assert!(
        result.atoms[0].atom.iterate.is_some(),
        "iterate spec must round-trip through the composer untouched"
    );
}
