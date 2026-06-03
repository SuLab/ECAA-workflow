//! WG2 — an archetype-author `ordering_only_edges` declaration is
//! honored by the DAG lift: a declared non-unifying `depends_on` edge
//! lifts as `EdgeKind::OrderingOnly` (non-blocking in Draft) and is
//! recorded as a *declared* `OrderingEdgeExempted` substrate row.
//! `single_cell_de` declares its `differential_expression depends_on
//! cell_type_annotation` edge ordering-only (DE's real data input is the
//! upstream normalized counts; the depends_on is a sequencing relation).
//!
//! NOTE: with the current v4 composer the scrnaseq goal's *winning*
//! composition is the generic_omics fallthrough, not the single_cell_de
//! scaffold — so the substrate test asserts the declaration fires during
//! the single_cell_de seed lift rather than relying on the winning dag's
//! final edge set. The "no Unproven edge survives into a validated dag"
//! guarantee is covered corpus-wide by `composer::validated_dag_edge_kinds`.

use std::collections::BTreeMap;
use std::path::Path;

use ecaa_workflow_core::archetype_registry::ArchetypeRegistry;
use ecaa_workflow_core::atom_registry::AtomRegistry;
use ecaa_workflow_core::composer::compose_with_modalities_full;
use ecaa_workflow_core::goal_spec::GoalSpec;

const ATOMS_DIR: &str = "../../config/stage-atoms";
const ARCHETYPES_DIR: &str = "../../config/archetypes";

fn regs() -> (AtomRegistry, ArchetypeRegistry) {
    (
        AtomRegistry::load_from_dir(Path::new(ATOMS_DIR)).expect("atoms"),
        ArchetypeRegistry::load_from_dir(Path::new(ARCHETYPES_DIR)).expect("archetypes"),
    )
}

/// Mirrors `composer_v4_scrnaseq_completeness::scrnaseq_annotation_goal`
/// — the goal whose composition exercises the `single_cell_de` seed.
fn scrnaseq_goal() -> GoalSpec {
    let mut modifiers = BTreeMap::new();
    modifiers.insert("kind".into(), "scrnaseq_annotation".into());
    GoalSpec {
        edam_data: "data:3917".into(),
        edam_format: Some("format:3590".into()),
        modifiers,
        source_prose: Some(
            "Single-cell RNA-seq clustering and cell type annotation across public \
             intervertebral disc datasets."
                .into(),
        ),
        confidence: 0.9,
    }
}

#[test]
fn single_cell_de_declares_the_de_ordering_edge() {
    let (_atoms, archetypes) = regs();
    // The author's declaration is loaded from
    // config/archetypes/single_cell_de.yaml and honored by the accessor
    // the lift loop consults.
    let scde = archetypes
        .get("single_cell_de")
        .expect("single_cell_de archetype loads");
    assert!(
        scde.is_ordering_only("cell_type_annotation", "differential_expression"),
        "the DE->cell_type ordering edge must be a declared exemption"
    );
    // An edge the author did NOT declare is not ordering-only — the
    // accessor is an exact-match lookup, not a wildcard.
    assert!(
        !scde.is_ordering_only("normalisation", "batch_correction"),
        "an undeclared edge must not report as ordering-only"
    );
}

#[test]
fn declared_ordering_edge_emits_a_declared_substrate_row() {
    use ecaa_workflow_core::decision_substrate::{drain, VerifierDecision};
    let _ = drain(); // clear the unscoped bucket
    let (atoms, archetypes) = regs();
    let goal = scrnaseq_goal();
    let _ = compose_with_modalities_full(
        &goal,
        "bioinformatics",
        &atoms,
        &archetypes,
        &["single_cell_rnaseq"],
        None,
        None,
        None,
    )
    .expect("compose");
    let rows = drain();
    let declared_de = rows.iter().any(|r| {
        matches!(
            r,
            VerifierDecision::OrderingEdgeExempted {
                producer_node,
                consumer_node,
                declared: true,
                ..
            } if producer_node == "cell_type_annotation"
                && consumer_node == "differential_expression"
        )
    });
    assert!(
        declared_de,
        "the single_cell_de DE edge must emit a *declared* OrderingEdgeExempted row \
         when its seed is lifted"
    );
}
