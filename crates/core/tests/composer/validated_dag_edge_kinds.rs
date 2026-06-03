//! WG5 — load-bearing invariant: every edge in a ValidatedExecutableDag
//! is TypedDataFlow, AdapterMediated, or a declared/synthesized
//! OrderingOnly. No Unproven edge may survive into an executable DAG.
//! Proven across the live archetype corpus so the ordering-edge loophole
//! stays closed under regression.

use std::collections::BTreeMap;
use std::path::Path;

use ecaa_workflow_core::archetype::ArchetypeDefinition;
use ecaa_workflow_core::archetype_registry::ArchetypeRegistry;
use ecaa_workflow_core::atom_registry::AtomRegistry;
use ecaa_workflow_core::composer::compose_with_modalities_full;
use ecaa_workflow_core::goal_spec::GoalSpec;
use ecaa_workflow_core::workflow_contracts::edge::EdgeKind;
use ecaa_workflow_core::workflow_contracts::outcome::ComposeOutcome;

// Negative control for the corpus property below lives as a `score_dag`
// unit test in `composer_v4::planner` (`score_dag_rejects_unproven_edge_\
// without_warning_text`): an Unproven edge Rejects regardless of warning
// text, which is the exact mechanism that keeps an undeclared
// non-unifying edge out of a ValidatedExecutableDag. Re-routing a
// synthetic broken archetype through the live registry matcher here would
// couple the test to matcher internals, so the unit-level control is the
// robust guard against this property passing trivially.

const ATOMS_DIR: &str = "../../config/stage-atoms";
const ARCHETYPES_DIR: &str = "../../config/archetypes";

fn regs() -> (AtomRegistry, ArchetypeRegistry) {
    (
        AtomRegistry::load_from_dir(Path::new(ATOMS_DIR)).expect("atoms"),
        ArchetypeRegistry::load_from_dir(Path::new(ARCHETYPES_DIR)).expect("archetypes"),
    )
}

fn goal_for(arch: &ArchetypeDefinition) -> GoalSpec {
    GoalSpec {
        edam_data: arch.goal_data.clone(),
        edam_format: arch.goal_format.clone(),
        modifiers: BTreeMap::new(),
        source_prose: None,
        confidence: 0.9,
    }
}

#[test]
fn every_validated_dag_edge_is_typed_adapter_or_declared_ordering() {
    let (atoms, archetypes) = regs();
    // Drive one composition per archetype using its declared goal +
    // modality_hint. Compositions that don't validate (return Err, i.e.
    // a non-executable outcome) are not this test's concern — the
    // invariant is about edges *inside a ValidatedExecutableDag*.
    for (id, arch) in archetypes.iter() {
        let goal = goal_for(arch);
        let modality = arch.modality_hint.clone();
        let modalities: Vec<&str> = match modality.as_deref() {
            Some(m) => vec![m],
            None => vec!["generic_omics"],
        };
        let out = match compose_with_modalities_full(
            &goal,
            &arch.project_class,
            &atoms,
            &archetypes,
            &modalities,
            None,
            None,
            None,
        ) {
            Ok(o) => o,
            Err(_) => continue, // non-executable outcome — not our concern
        };
        if !matches!(
            out.compose_outcome,
            Some(ComposeOutcome::ValidatedExecutableDag { .. })
        ) {
            continue;
        }
        let dag = out
            .workflow_dag
            .expect("a validated executable dag carries a WorkflowDag");
        for e in &dag.edges {
            assert_ne!(
                e.kind,
                EdgeKind::Unproven,
                "archetype {id}: edge {}->{} is Unproven in a ValidatedExecutableDag — \
                 the gate let an untyped edge through (loophole reopened)",
                e.from_node,
                e.to_node,
            );
        }
    }
}
