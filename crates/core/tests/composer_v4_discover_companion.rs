//! Closure Phase B.3 — `discover_*` companion synthesis for v4.
//!
//! v2 builder's `emit_stage` post-pass synthesizes a `validate_<id>`
//! after every result-producing stage. The analogous `discover_<axis>`
//! companion was never wired in v4, which is why 8 conversation
//! fixtures had to pin `composer_version: 1` to keep their
//! `set_intake_method` / `IntakeFollowup` assertions firing. This
//! test guards the post-pass insertion: any v4-built DAG that
//! contains operation atoms with runtime method discovery (either an
//! explicit `method_choice.deferred_to` field OR an
//! `attributes.candidate_tools` list) must surface a
//! `discover_<axis>` companion so the SME has a node to pin
//! `set_intake_method` against.
//!
//! See Task B.3 in the DAG 100-percent-closure design for the full spec.

use scripps_workflow_core::archetype_registry::ArchetypeRegistry;
use scripps_workflow_core::atom_registry::AtomRegistry;
use scripps_workflow_core::backend_emitters::{lower_to_workflow_json, EmitContext};
use scripps_workflow_core::composer::compose_with_version_and_modalities_full;
use scripps_workflow_core::dag::{DiscoveryKind, TaskKind};
use scripps_workflow_core::goal_spec::GoalSpec;

#[test]
fn v4_dag_emits_discover_companion_for_method_choice_atoms() {
    let atom_reg =
        AtomRegistry::load_from_dir(std::path::Path::new("../../config/stage-atoms")).unwrap();
    let archetype_reg =
        ArchetypeRegistry::load_from_dir(std::path::Path::new("../../config/archetypes")).unwrap();

    // scRNA-seq DE path: the archetype canonically threads through
    // `batch_correction`, `clustering`, `normalisation`, `alignment`,
    // `differential_expression` — all of which carry
    // `attributes.candidate_tools`, so the synthesis pass must surface
    // a `discover_<id>` companion for each.
    // scRNA-seq archetype `single_cell_de` advertises goal_data
    // `data:3917` / goal_format `format:3590` (Expression matrix /
    // HDF5). Match that so the archetype seed fires.
    let goal = GoalSpec {
        edam_data: "data:3917".into(),
        edam_format: Some("format:3590".into()),
        modifiers: Default::default(),
        source_prose: Some("single-cell RNA-seq differential expression".into()),
        confidence: 0.9,
    };

    let result = compose_with_version_and_modalities_full(
        &goal,
        "research",
        &atom_reg,
        &archetype_reg,
        4,
        &["single_cell_rnaseq"],
        None,
        None,
        None,
    )
    .expect("compose Ok");

    let workflow_dag = result
        .workflow_dag
        .as_ref()
        .expect("v4 path must populate ComposerOutput.workflow_dag");

    let discover_nodes: Vec<&str> = workflow_dag
        .nodes
        .iter()
        .filter(|n| n.id.starts_with("discover_"))
        .map(|n| n.id.as_str())
        .collect();

    assert!(
        !discover_nodes.is_empty(),
        "v4 must synthesize discover_* companion(s) for method-choice atoms in the scRNA-seq DE \
         DAG. Without discover_* nodes, set_intake_method calls (fixtures 12, 20, 28) fail \
         validation and the IntakeFollowup state-trigger never fires (fixtures 08, 09, 22, 23). \
         Got node ids: {:?}",
        workflow_dag.nodes.iter().map(|n| &n.id).collect::<Vec<_>>()
    );

    // Determinism guarantee: nodes are sorted by id after synthesis
    // insertion (see planner.rs post-meet_in_middle sort). Verify the
    // ordering here so future drift surfaces as a test failure rather
    // than a byte-diff regression.
    let mut sorted = workflow_dag
        .nodes
        .iter()
        .map(|n| n.id.clone())
        .collect::<Vec<_>>();
    sorted.sort();
    let actual = workflow_dag
        .nodes
        .iter()
        .map(|n| n.id.clone())
        .collect::<Vec<_>>();
    assert_eq!(
        actual, sorted,
        "v4 dag.nodes must be sorted by id for byte-determinism after discover-companion synthesis"
    );
}

/// v4-synthesized `discover_*` companions must lower to
/// `DiscoveryKind::BestPractice` (not `Custom("custom")`) and must
/// set `requires_sme_review = true` in the emitted `WORKFLOW.json`.
/// `TaskNode::synthesize_discover` must set the `discovery_kind`
/// attribute; if omitted, lowering falls through to
/// `DiscoveryKind::Custom("custom")` (a silent correctness defect).
/// The SME gate (`filter_picks_respecting_sme_gate`) was dead code for
/// v4 DAGs because no synthesized companion carried the
/// `requires_sme_review` precondition.
#[test]
fn synthesized_discover_companions_lower_to_best_practice_and_sme_review() {
    let atom_reg =
        AtomRegistry::load_from_dir(std::path::Path::new("../../config/stage-atoms")).unwrap();
    let archetype_reg =
        ArchetypeRegistry::load_from_dir(std::path::Path::new("../../config/archetypes")).unwrap();

    let goal = GoalSpec {
        edam_data: "data:3917".into(),
        edam_format: Some("format:3590".into()),
        modifiers: Default::default(),
        source_prose: Some("single-cell RNA-seq differential expression".into()),
        confidence: 0.9,
    };

    let result = compose_with_version_and_modalities_full(
        &goal,
        "research",
        &atom_reg,
        &archetype_reg,
        4,
        &["single_cell_rnaseq"],
        None,
        None,
        None,
    )
    .expect("compose Ok");

    let workflow_dag = result
        .workflow_dag
        .as_ref()
        .expect("v4 path must populate ComposerOutput.workflow_dag");

    // Lower to WORKFLOW.json so we can assert on the emitted Task fields.
    let ctx = EmitContext::defaults();
    let artifact = lower_to_workflow_json(workflow_dag, &ctx)
        .expect("lower_to_workflow_json must succeed on a well-formed v4 DAG");
    let dag = artifact.dag;

    let discover_tasks: Vec<(
        &scripps_workflow_core::dag::TaskId,
        &scripps_workflow_core::dag::Task,
    )> = dag
        .tasks
        .iter()
        .filter(|(id, _)| id.as_str().starts_with("discover_"))
        .collect();

    assert!(
        !discover_tasks.is_empty(),
        "No discover_* tasks found in lowered DAG. Node ids: {:?}",
        dag.tasks.keys().collect::<Vec<_>>()
    );

    for (task_id, task) in &discover_tasks {
        // Fix 1: must lower to BestPractice, not Custom("custom").
        assert!(
            matches!(task.kind, TaskKind::Discovery(DiscoveryKind::BestPractice)),
            "Task '{}' must have DiscoveryKind::BestPractice but got {:?}. \
             synthesize_discover must set discovery_kind = \"best_practice\" \
             so the lowering pass doesn't fall through to Custom.",
            task_id,
            task.kind
        );

        // Fix 3: must set requires_sme_review = true so the scheduler gate fires.
        assert!(
            task.requires_sme_review,
            "Task '{}' must have requires_sme_review = true. \
             Without this precondition the SME-review filter is dead code for v4 DAGs.",
            task_id
        );
    }
}
