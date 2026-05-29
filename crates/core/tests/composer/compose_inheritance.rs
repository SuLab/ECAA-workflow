//! `Composer::resolve_inheritance` flattens `compose:`
//! directives recursively.
//!
//! Validates the inheritance contract:
//! - `position: prefix` prepends the inherited archetype's atoms.
//! - `position: suffix` appends.
//! - `position: replace_atoms` replaces this archetype's own atom
//! list entirely with the inherited one (less common; used when
//! the deriving archetype only adds metadata).
//! - `id_prefix:` namespaces every inherited stage-id (alias /
//! atom_id) AND rewrites its `depends_on` references so the
//! inherited DAG stays internally consistent.
//! - `replace_atoms:` substitutes specific atoms within the inherited
//! archetype.
//! - Cycle detection rejects A inherits B inherits A.
//! - Depth cap (4) rejects deeper chains.

use ecaa_workflow_core::archetype::{
    ArchetypeAtomRef, ArchetypeDefinition, ComposePosition, ComposeRef,
    CURRENT_ARCHETYPE_SCHEMA_VERSION,
};
use ecaa_workflow_core::archetype_registry::ArchetypeRegistry;
use ecaa_workflow_core::composer::{
    resolve_inheritance, CompositionError, INHERITANCE_DEPTH_CAP,
};
use std::collections::BTreeMap;

fn arch(id: &str, atoms: Vec<ArchetypeAtomRef>, compose: Vec<ComposeRef>) -> ArchetypeDefinition {
    ArchetypeDefinition {
        schema_version: CURRENT_ARCHETYPE_SCHEMA_VERSION.into(),
        id: id.to_string(),
        version: "1.0.0".into(),
        description: format!("test archetype {id}"),
        sme_summary: "test".into(),
        goal_data: "data:0951".into(),
        goal_format: Some("format:3475".into()),
        atoms,
        slot_mappings: BTreeMap::new(),
        compose,
        slots: None,
        cross_dependencies: vec![],
        claim_boundary: None,
        project_class: "bioinformatics".into(),
        modality_hint: None,
        goal_kind_hint: None,
        preferred_container: None,
        runtime_baseline: Default::default(),
        cross_omics_modalities: vec![],
    }
}

fn atom_ref(atom_id: &str, deps: Vec<&str>) -> ArchetypeAtomRef {
    ArchetypeAtomRef {
        atom_id: atom_id.into(),
        alias: None,
        depends_on: deps.iter().map(|s| s.to_string()).collect(),
        required: true,
        required_figures: None,
        plot_stage_id: None,
        figure_exempt: None,
        expected_artifacts: None,
        required_artifacts: None,
    }
}

fn registry_from(archetypes: Vec<ArchetypeDefinition>) -> ArchetypeRegistry {
    ArchetypeRegistry::test_from_archetypes(archetypes)
}

#[test]
fn flattens_prefix_inheritance() {
    let parent = arch(
        "parent",
        vec![
            atom_ref("alignment", vec![]),
            atom_ref("quantification", vec!["alignment"]),
        ],
        vec![],
    );
    let child = arch(
        "child",
        vec![ArchetypeAtomRef {
            atom_id: "reporting".into(),
            alias: Some("final_report".into()),
            depends_on: vec!["rnaseq_quantification".into()],
            required: true,
            required_figures: None,
            plot_stage_id: None,
            figure_exempt: None,
            expected_artifacts: None,
            required_artifacts: None,
        }],
        vec![ComposeRef {
            archetype_id: "parent".into(),
            position: ComposePosition::Prefix,
            id_prefix: Some("rnaseq_".into()),
            replace_atoms: BTreeMap::new(),
        }],
    );
    let reg = registry_from(vec![parent, child.clone()]);
    let flat = resolve_inheritance(&child, &reg).expect("resolve must succeed");
    let stage_ids: Vec<&str> = flat
        .atoms
        .iter()
        .map(|a| a.alias.as_deref().unwrap_or(a.atom_id.as_str()))
        .collect();
    assert_eq!(
        stage_ids,
        vec!["rnaseq_alignment", "rnaseq_quantification", "final_report"],
        "inherited atoms should be prefixed and prepended; own atoms follow"
    );

    // depends_on of inherited atoms should be prefix-rewritten too.
    let quant = flat
        .atoms
        .iter()
        .find(|a| a.alias.as_deref() == Some("rnaseq_quantification"))
        .expect("rnaseq_quantification must exist");
    assert_eq!(
        quant.depends_on,
        vec!["rnaseq_alignment"],
        "inherited depends_on must be prefix-rewritten"
    );
}

#[test]
fn flattens_two_inherited_archetypes_with_distinct_prefixes() {
    let bulk = arch(
        "bulk_rnaseq",
        vec![
            atom_ref("alignment", vec![]),
            atom_ref("differential_expression", vec!["alignment"]),
        ],
        vec![],
    );
    let prot = arch(
        "proteomics",
        vec![
            atom_ref("peptide_search", vec![]),
            atom_ref("differential_expression", vec!["peptide_search"]),
        ],
        vec![],
    );
    let cross = arch(
        "cross_omics",
        vec![ArchetypeAtomRef {
            atom_id: "reporting".into(),
            alias: Some("thematic".into()),
            depends_on: vec![
                "rnaseq_differential_expression".into(),
                "proteomics_differential_expression".into(),
            ],
            required: true,
            required_figures: None,
            plot_stage_id: None,
            figure_exempt: None,
            expected_artifacts: None,
            required_artifacts: None,
        }],
        vec![
            ComposeRef {
                archetype_id: "bulk_rnaseq".into(),
                position: ComposePosition::Prefix,
                id_prefix: Some("rnaseq_".into()),
                replace_atoms: BTreeMap::new(),
            },
            ComposeRef {
                archetype_id: "proteomics".into(),
                position: ComposePosition::Prefix,
                id_prefix: Some("proteomics_".into()),
                replace_atoms: BTreeMap::new(),
            },
        ],
    );
    let reg = registry_from(vec![bulk, prot, cross.clone()]);
    let flat = resolve_inheritance(&cross, &reg).expect("must resolve");
    let stage_ids: Vec<&str> = flat
        .atoms
        .iter()
        .map(|a| a.alias.as_deref().unwrap_or(a.atom_id.as_str()))
        .collect();
    assert_eq!(
        stage_ids,
        vec![
            "rnaseq_alignment",
            "rnaseq_differential_expression",
            "proteomics_peptide_search",
            "proteomics_differential_expression",
            "thematic",
        ],
        "two-arch prefix inheritance must produce disjoint namespaces in order"
    );
}

#[test]
fn replace_atoms_substitutes_named_stage() {
    let parent = arch(
        "parent",
        vec![
            atom_ref("alignment", vec![]),
            atom_ref("quantification", vec!["alignment"]),
        ],
        vec![],
    );
    let mut replace = BTreeMap::new();
    replace.insert("quantification".into(), "salmon_quant".into());
    let child = arch(
        "child",
        vec![],
        vec![ComposeRef {
            archetype_id: "parent".into(),
            position: ComposePosition::Prefix,
            id_prefix: None,
            replace_atoms: replace,
        }],
    );
    let reg = registry_from(vec![parent, child.clone()]);
    let flat = resolve_inheritance(&child, &reg).expect("must resolve");
    let atom_ids: Vec<&str> = flat.atoms.iter().map(|a| a.atom_id.as_str()).collect();
    assert_eq!(
        atom_ids,
        vec!["alignment", "salmon_quant"],
        "replace_atoms must swap quantification → salmon_quant"
    );
}

#[test]
fn cycle_detection_rejects_circular_inheritance() {
    let a = arch(
        "a",
        vec![atom_ref("x", vec![])],
        vec![ComposeRef {
            archetype_id: "b".into(),
            position: ComposePosition::Prefix,
            id_prefix: None,
            replace_atoms: BTreeMap::new(),
        }],
    );
    let b = arch(
        "b",
        vec![atom_ref("y", vec![])],
        vec![ComposeRef {
            archetype_id: "a".into(),
            position: ComposePosition::Prefix,
            id_prefix: None,
            replace_atoms: BTreeMap::new(),
        }],
    );
    let reg = registry_from(vec![a.clone(), b]);
    let err = resolve_inheritance(&a, &reg).unwrap_err();
    assert!(
        matches!(err, CompositionError::InheritanceCycle { .. }),
        "expected InheritanceCycle, got {err:?}"
    );
}

#[test]
fn depth_cap_rejects_deep_inheritance() {
    // Build a chain a -> b -> c -> d -> e -> f (5 levels of nesting,
    // exceeds the cap of INHERITANCE_DEPTH_CAP=4).
    let last = arch("f", vec![atom_ref("z", vec![])], vec![]);
    let mut prev_id = "f".to_string();
    let mut chain: Vec<ArchetypeDefinition> = vec![last];
    for id in ["e", "d", "c", "b", "a"] {
        let a = arch(
            id,
            vec![atom_ref(&format!("op_{id}"), vec![])],
            vec![ComposeRef {
                archetype_id: prev_id.clone(),
                position: ComposePosition::Prefix,
                id_prefix: None,
                replace_atoms: BTreeMap::new(),
            }],
        );
        chain.push(a);
        prev_id = id.to_string();
    }
    let head = chain.last().cloned().unwrap();
    let reg = registry_from(chain);
    let err = resolve_inheritance(&head, &reg).unwrap_err();
    assert!(
        matches!(err, CompositionError::InheritanceDepthExceeded { .. }),
        "expected InheritanceDepthExceeded, got {err:?}"
    );
    assert_eq!(INHERITANCE_DEPTH_CAP, 4);
}

#[test]
fn unknown_inherited_archetype_surfaces_typed_error() {
    let child = arch(
        "child",
        vec![atom_ref("x", vec![])],
        vec![ComposeRef {
            archetype_id: "ghost".into(),
            position: ComposePosition::Prefix,
            id_prefix: None,
            replace_atoms: BTreeMap::new(),
        }],
    );
    let reg = registry_from(vec![child.clone()]);
    let err = resolve_inheritance(&child, &reg).unwrap_err();
    assert!(
        matches!(err, CompositionError::UnknownInheritedArchetype { .. }),
        "expected UnknownInheritedArchetype, got {err:?}"
    );
}

#[test]
fn unknown_replace_target_surfaces_typed_error() {
    let parent = arch("parent", vec![atom_ref("foo", vec![])], vec![]);
    let mut replace = BTreeMap::new();
    replace.insert("nonexistent".into(), "replacement".into());
    let child = arch(
        "child",
        vec![],
        vec![ComposeRef {
            archetype_id: "parent".into(),
            position: ComposePosition::Prefix,
            id_prefix: None,
            replace_atoms: replace,
        }],
    );
    let reg = registry_from(vec![parent, child.clone()]);
    let err = resolve_inheritance(&child, &reg).unwrap_err();
    assert!(
        matches!(err, CompositionError::UnknownReplaceTarget { .. }),
        "expected UnknownReplaceTarget, got {err:?}"
    );
}

#[test]
fn no_compose_directives_returns_atoms_verbatim() {
    let plain = arch(
        "plain",
        vec![atom_ref("a", vec![]), atom_ref("b", vec!["a"])],
        vec![],
    );
    let reg = registry_from(vec![plain.clone()]);
    let flat = resolve_inheritance(&plain, &reg).expect("must resolve");
    let atom_ids: Vec<&str> = flat.atoms.iter().map(|a| a.atom_id.as_str()).collect();
    assert_eq!(atom_ids, vec!["a", "b"]);
    assert!(flat.lineage.is_empty(), "no compose: → empty lineage");
}
