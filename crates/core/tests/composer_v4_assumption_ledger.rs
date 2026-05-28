//! Integration guard: the v4 planner must populate `WorkflowDag.assumptions`
//! with at least one entry for any real planning session.
//!
//! Without this guard the v4 planner emits `WorkflowDag`s carrying
//! `assumptions: AssumptionLedger { entries: [] }` regardless of
//! the planning path, leaving the grant's "uncertainty ledger"
//! surface empty. These tests assert that for a representative
//! bulk-RNA-seq goal:
//!
//! - Every alternative carries at least one assumption entry.
//! - The archetype-seed alternative carries `RegistryDefault` assumptions.
//! - The search-seed alternative carries a `SeedHeuristic` assumption.

use scripps_workflow_core::archetype_registry::ArchetypeRegistry;
use scripps_workflow_core::atom_registry::AtomRegistry;
use scripps_workflow_core::composer_v4::{plan, PlanningContext};
use scripps_workflow_core::goal_spec::GoalSpec;
use scripps_workflow_core::workflow_contracts::{
    data_product::DataProductContract,
    evidence::AssumptionSource,
    workflow_intent::{DesiredOutput, WorkflowIntent},
};

const ATOMS_DIR: &str = "../../config/stage-atoms";
const ARCHETYPES_DIR: &str = "../../config/archetypes";

fn bulk_rnaseq_ctx() -> (GoalSpec, PlanningContext) {
    let de_goal = DataProductContract::sample_de_table();
    let goal_iri = de_goal.semantic_type.stable_id();

    let goal = GoalSpec {
        edam_data: goal_iri.clone(),
        edam_format: Some("format:3475".into()),
        modifiers: Default::default(),
        source_prose: Some("Bulk RNA-seq differential expression, IBD cohort".into()),
        confidence: 0.9,
    };

    let intent = WorkflowIntent {
        id: "assumption_ledger_test".into(),
        schema_version: semver::Version::new(1, 0, 0),
        goal: "bulk RNA-seq DE".into(),
        modality: Some("bulk_rnaseq".into()),
        project_class: Some("research".into()),
        available_data: vec![DataProductContract::sample_paired_fastq()],
        desired_outputs: vec![DesiredOutput {
            label: "differential expression table".into(),
            edam_data: Some(goal_iri),
            edam_format: Some("format:3475".into()),
            human_readable: false,
        }],
        ..Default::default()
    };

    let mut ctx = PlanningContext::new(intent);
    ctx.max_branches = 64;
    ctx.max_depth = 8;
    ctx.max_alternatives = 5;

    (goal, ctx)
}

/// Every alternative produced by the planner must carry a non-empty
/// assumption ledger — this is the load-bearing assertion that the
/// grant's "uncertainty ledger" surface ships non-empty for any real
/// planning session.
#[test]
fn plan_populates_assumption_ledger_for_bulk_rnaseq_goal() {
    let atom_reg = AtomRegistry::load_from_dir(std::path::Path::new(ATOMS_DIR))
        .expect("AtomRegistry must load from config/stage-atoms");
    let archetype_reg = ArchetypeRegistry::load_from_dir(std::path::Path::new(ARCHETYPES_DIR))
        .expect("ArchetypeRegistry must load from config/archetypes");

    let (goal, ctx) = bulk_rnaseq_ctx();
    let result = plan(&ctx, &goal, "research", &atom_reg, &archetype_reg);
    let alts = &result.alternatives;

    assert!(
        !alts.is_empty(),
        "expected ≥1 alternative for bulk-RNA-seq DE goal; primary={:?}",
        result.primary
    );

    for (i, alt) in alts.iter().enumerate() {
        assert!(
            !alt.dag.assumptions.entries.is_empty(),
            "alternative #{i} (source='{}') has an empty assumption ledger; \
             the planner must populate it at planning-decision sites. \
             dag.id='{}' nodes={} edges={}",
            alt.source,
            alt.dag.id,
            alt.dag.nodes.len(),
            alt.dag.edges.len(),
        );
    }
}

/// The archetype-seed alternative must carry `RegistryDefault` entries —
/// one per atom that was added from the archetype catalog without an
/// explicit user configuration override.
#[test]
fn archetype_alternative_has_registry_default_assumptions() {
    let atom_reg = AtomRegistry::load_from_dir(std::path::Path::new(ATOMS_DIR))
        .expect("AtomRegistry must load from config/stage-atoms");
    let archetype_reg = ArchetypeRegistry::load_from_dir(std::path::Path::new(ARCHETYPES_DIR))
        .expect("ArchetypeRegistry must load from config/archetypes");

    let (goal, ctx) = bulk_rnaseq_ctx();
    let result = plan(&ctx, &goal, "research", &atom_reg, &archetype_reg);

    // Find the archetype-seed alternative.
    let Some(alt) = result.alternatives.iter().find(|a| a.source == "archetype") else {
        // No archetype seed fired for this goal — skip the assertion.
        return;
    };

    let registry_default_count = alt
        .dag
        .assumptions
        .entries
        .iter()
        .filter(|a| matches!(a.source, AssumptionSource::RegistryDefault { .. }))
        .count();

    assert!(
        registry_default_count > 0,
        "archetype alternative must carry ≥1 RegistryDefault assumption. \
         dag.nodes={} assumption_entries={}",
        alt.dag.nodes.len(),
        alt.dag.assumptions.entries.len(),
    );
}

/// The search-seed alternative must carry a `SeedHeuristic` entry
/// recording the forward/backward/meet-in-middle strategy used to
/// assemble the DAG.
#[test]
fn search_alternative_has_seed_heuristic_assumption() {
    let atom_reg = AtomRegistry::load_from_dir(std::path::Path::new(ATOMS_DIR))
        .expect("AtomRegistry must load from config/stage-atoms");
    let archetype_reg = ArchetypeRegistry::load_from_dir(std::path::Path::new(ARCHETYPES_DIR))
        .expect("ArchetypeRegistry must load from config/archetypes");

    let (goal, ctx) = bulk_rnaseq_ctx();
    let result = plan(&ctx, &goal, "research", &atom_reg, &archetype_reg);

    // Find the search-seed alternative (Disconnected meet → no alternative added).
    let Some(alt) = result.alternatives.iter().find(|a| a.source == "search") else {
        return;
    };

    let seed_heuristic_count = alt
        .dag
        .assumptions
        .entries
        .iter()
        .filter(|a| matches!(a.source, AssumptionSource::SeedHeuristic { .. }))
        .count();

    assert!(
        seed_heuristic_count > 0,
        "search alternative must carry ≥1 SeedHeuristic assumption. \
         assumption_entries={:?}",
        alt.dag.assumptions.entries,
    );
}
