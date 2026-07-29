//! Universal literature contextualization.
//!
//! Literature grounding is unconditional: every archetype must surface
//! `contextualize_findings_with_literature` (+ its `review_prior_work`
//! upstream) in every emitted DAG, regardless of intake keywords. The
//! opt-in gate that previously dropped these atoms on plain prose has been
//! removed.
//!
//! Two complementary checks:
//!
//! 1. `every_archetype_declares_literature_atoms` — a routing-independent
//!    config invariant: each archetype either declares the literature
//!    family inline (or in its slot manifest) OR inherits it via a
//!    `compose:` from a sub-archetype that declares it. This is the
//!    structural guarantee behind "present in EVERY DAG".
//!
//! 2. `representative_archetypes_compose_contextualize` — composes a
//!    spread of single-modality + cross-omics archetypes via the v4
//!    planner and asserts the literature atom materializes in the lowered
//!    DAG. (The full deterministic-intake coverage across all archetypes
//!    is the blinded DAG-correctness corpus, which the
//!    `prune_literature_atoms_from_workflow_dag` no-op now lets through
//!    with the literature family intact.)

use ecaa_workflow_core::archetype::ArchetypeDefinition;
use ecaa_workflow_core::archetype_registry::ArchetypeRegistry;
use ecaa_workflow_core::atom_registry::AtomRegistry;
use ecaa_workflow_core::composer::compose_with_modalities_full;
use ecaa_workflow_core::goal_spec::GoalSpec;
use std::collections::BTreeSet;
use std::path::Path;

const CONTEXTUALIZE: &str = "contextualize_findings_with_literature";
const REVIEW_PRIOR: &str = "review_prior_work";

fn workspace_config() -> (AtomRegistry, ArchetypeRegistry) {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let workspace = manifest.parent().unwrap().parent().unwrap();
    let atoms =
        AtomRegistry::load_from_dir(&workspace.join("config/stage-atoms")).expect("load atoms");
    let archetypes = ArchetypeRegistry::load_from_dir(&workspace.join("config/archetypes"))
        .expect("load archetypes");
    (atoms, archetypes)
}

/// True when the archetype declares an atom with the given id, either
/// directly in `atoms:` or in its slot manifest's `extra_atoms`.
fn declares_atom(arch: &ArchetypeDefinition, atom_id: &str) -> bool {
    if arch.atoms.iter().any(|a| a.atom_id.as_str() == atom_id) {
        return true;
    }
    if let Some(slots) = arch.slots.as_ref() {
        if slots
            .values
            .iter()
            .any(|v| v.extra_atoms.iter().any(|e| e.atom_id.as_str() == atom_id))
        {
            return true;
        }
    }
    false
}

/// Recursively: does `arch` declare `atom_id` directly, or inherit it via
/// any `compose:` sub-archetype that (transitively) declares it?
fn declares_or_inherits(
    arch: &ArchetypeDefinition,
    atom_id: &str,
    reg: &ArchetypeRegistry,
    seen: &mut BTreeSet<String>,
) -> bool {
    if declares_atom(arch, atom_id) {
        return true;
    }
    if !seen.insert(arch.id.clone()) {
        return false;
    }
    for c in &arch.compose {
        if let Some(sub) = reg.get(&c.archetype_id) {
            if declares_or_inherits(sub, atom_id, reg, seen) {
                return true;
            }
        }
    }
    false
}

/// Config invariant: every archetype declares (or inherits via compose)
/// both literature atoms.
#[test]
fn every_archetype_declares_literature_atoms() {
    let (_atoms, archetypes) = workspace_config();
    assert!(
        !archetypes.is_empty(),
        "archetype registry must load at least one archetype"
    );

    let mut missing: Vec<String> = Vec::new();
    let mut checked = 0usize;
    for (id, arch) in archetypes.iter() {
        checked += 1;
        let has_ctx = declares_or_inherits(arch, CONTEXTUALIZE, &archetypes, &mut BTreeSet::new());
        let has_rpw = declares_or_inherits(arch, REVIEW_PRIOR, &archetypes, &mut BTreeSet::new());
        if !has_ctx || !has_rpw {
            missing.push(format!(
                "{id} (contextualize={has_ctx}, review_prior_work={has_rpw})"
            ));
        }
    }

    assert!(
        missing.is_empty(),
        "every archetype must declare/inherit both literature atoms \
         ({checked} archetypes checked); missing in: {missing:#?}"
    );
}

/// Compose a representative spread of archetypes via the v4 planner and
/// assert the literature contextualization atom materializes in the
/// composed DAG (bare or namespaced for cross-omics).
#[test]
fn representative_archetypes_compose_contextualize() {
    let (atoms, archetypes) = workspace_config();

    // (archetype-driving prose, modality slice). Picked across families:
    // peak-calling, DE, clinical, single-cell, cross-omics.
    let cases: &[(&str, &str, Option<&str>, &[&str])] = &[
        (
            "data:0951",
            "format:3475",
            Some("bulk_rnaseq"),
            &["bulk_rnaseq"],
        ),
        ("data:1255", "format:3003", Some("chip_seq"), &["chip_seq"]),
        ("data:1255", "format:3003", Some("atac_seq"), &["atac_seq"]),
        (
            "data:3498",
            "format:3016",
            Some("variant_calling"),
            &["variant_calling"],
        ),
        (
            "data:0006",
            "format:3475",
            Some("generic_omics"),
            &["generic_omics"],
        ),
    ];

    for (edam_data, edam_format, prose_modality, modalities) in cases {
        let goal = GoalSpec {
            edam_data: (*edam_data).into(),
            edam_format: Some((*edam_format).into()),
            modifiers: Default::default(),
            source_prose: prose_modality.map(|m| format!("Analyze {m} data and report findings.")),
            confidence: 0.9,
        };
        let result = compose_with_modalities_full(
            &goal,
            "bioinformatics",
            &atoms,
            &archetypes,
            modalities,
            None,
            None,
            None,
        )
        .unwrap_or_else(|e| panic!("compose failed for modalities {modalities:?}: {e:?}"));
        let ids: BTreeSet<&str> = result
            .composition
            .atoms
            .iter()
            .map(|c| c.stage_id.as_str())
            .collect();
        let has_ctx = ids
            .iter()
            .any(|n| *n == CONTEXTUALIZE || n.ends_with("_contextualize_findings_with_literature"));
        assert!(
            has_ctx,
            "composed DAG for modalities {modalities:?} must contain a contextualization atom; \
             got {ids:?}"
        );
    }
}
