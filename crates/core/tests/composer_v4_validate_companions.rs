//! Regression: v4 emissions must synthesize
//! `validate_*` companion stages for every result-producing node in
//! the planner's `WorkflowDag`, mirroring the v2 builder's
//! validate-companion post-pass (`builder.rs::emit_stage`).
//!
//! v2 carries both the operation atoms AND a `validate_<id>` task
//! per non-validation/non-review stage. The v4 planner runs a small
//! post-pass after `meet_in_the_middle` (and after the archetype
//! seed lifts a `WorkflowDag`) that walks every result-producing
//! node and adds a `validate_<id>` `TaskNode` plus a downstream
//! edge wiring it as a consumer of the original node. Without this
//! pass the v4 emission would contain only the core operation
//! atoms (`differential_expression`, `variant_calling`,
//! `clustering`).
//!
//! The contract is intentionally generous: the validator atom does
//! NOT need to live in the registry. The companion is synthesized
//! from the producer's id, role-stamped as `Validation`, and lowered
//! by `lower_to_workflow_json` into a `Task { kind: TaskKind::Validation }`.
//! This matches v2's behavior — `validate_<id>` is a generic
//! "validate the upstream stage's outputs" task, not a separately-
//! authored atom.
//!
//! Three flagship assertions exercise the bulk-rnaseq, scrnaseq, and
//! variant-calling scenarios from the parity corpus:
//!
//! - **bulk-rnaseq** — `differential_expression` must have its
//! `validate_differential_expression` companion.
//! - **scrnaseq** — `clustering` must have its `validate_clustering`
//! companion (or, if the planner's particular alternative didn't
//! surface clustering, fall back to checking that *some* validate
//! companion is present per result-producing node).
//! - **variant-calling** — `variant_calling` must have its
//! `validate_variant_calling` companion.
//!
//! Adapter atoms (lossless port adapters inserted by the engine) and
//! atoms whose role is already `Validation` / `Discovery` must NOT
//! receive a companion. Aggregator atoms (e.g. `integration`) DO get
//! companions in v4 — the skip rules are aligned with
//! v2's `emit_stage` post-pass, which only skips on `is_review ||
//! is_validation || EmpiricalRequired` (not on the typed
//! `Aggregator` role).

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use scripps_workflow_core::archetype_registry::ArchetypeRegistry;
use scripps_workflow_core::atom_registry::AtomRegistry;
use scripps_workflow_core::composer_v4::{plan as v4_plan, PlanningContext};
use scripps_workflow_core::goal_spec::GoalSpec;
use scripps_workflow_core::workflow_contracts::data_product::DataProductContract;
use scripps_workflow_core::workflow_contracts::outcome::ComposeOutcome;
use scripps_workflow_core::workflow_contracts::task_node::WorkflowDag;
use scripps_workflow_core::workflow_contracts::workflow_intent::{DesiredOutput, WorkflowIntent};

const ATOMS_DIR: &str = "../../config/stage-atoms";
const ARCHETYPES_DIR: &str = "../../config/archetypes";

fn bulk_rnaseq_de_goal() -> GoalSpec {
    GoalSpec {
        edam_data: "data:0951".into(),
        edam_format: Some("format:3475".into()),
        modifiers: BTreeMap::new(),
        source_prose: Some(
            "Bulk RNA-seq differential expression analysis on an IBD cohort, \
             responder vs non-responder."
                .into(),
        ),
        confidence: 0.9,
    }
}

fn scrnaseq_clustering_goal() -> GoalSpec {
    GoalSpec {
        edam_data: "data:3917".into(),
        edam_format: Some("format:3590".into()),
        modifiers: BTreeMap::new(),
        source_prose: Some(
            "Single-cell RNA-seq clustering and cell type annotation across public \
             intervertebral disc datasets."
                .into(),
        ),
        confidence: 0.9,
    }
}

fn variant_calling_goal() -> GoalSpec {
    GoalSpec {
        edam_data: "data:3498".into(),
        edam_format: Some("format:3016".into()),
        modifiers: BTreeMap::new(),
        source_prose: Some(
            "Germline short variant calling on the GIAB HG002 30x Illumina WGS \
             reference sample with BWA + GATK4 HaplotypeCaller."
                .into(),
        ),
        confidence: 0.9,
    }
}

/// Drive the v4 planner with a hand-crafted intent (mirrors the
/// parity-corpus regenerator + `composer_v4_no_cycles.rs` shape) and
/// return the PRIMARY alternative's WorkflowDag. We exercise the
/// `WorkflowDag` directly (not the lowered `DAG`) because Task D's
/// post-pass operates at the IR level — testing on `WorkflowDag`
/// keeps the assertion local to the synthesis site rather than
/// reaching through the lowering.
fn run_v4_planner(modality: &str, goal: &GoalSpec) -> WorkflowDag {
    let atom_reg = AtomRegistry::load_from_dir(Path::new(ATOMS_DIR))
        .expect("AtomRegistry must load from config/stage-atoms");
    let archetype_reg = ArchetypeRegistry::load_from_dir(Path::new(ARCHETYPES_DIR))
        .expect("ArchetypeRegistry must load from config/archetypes");
    let intent = WorkflowIntent {
        id: format!("v4_validate_companions_{modality}"),
        schema_version: semver::Version::new(1, 0, 0),
        goal: goal
            .source_prose
            .clone()
            .unwrap_or_else(|| goal.edam_data.clone()),
        modality: Some(modality.into()),
        project_class: Some("bioinformatics".into()),
        available_data: vec![DataProductContract::sample_paired_fastq()],
        desired_outputs: vec![DesiredOutput {
            label: goal
                .source_prose
                .clone()
                .unwrap_or_else(|| goal.edam_data.clone()),
            edam_data: Some(goal.edam_data.clone()),
            edam_format: goal.edam_format.clone(),
            human_readable: false,
        }],
        ..Default::default()
    };
    let mut ctx = PlanningContext::new(intent);
    ctx.max_branches = 64;
    ctx.max_depth = 12;
    ctx.max_alternatives = 5;

    let result = v4_plan(&ctx, goal, "bioinformatics", &atom_reg, &archetype_reg);
    match result.primary {
        ComposeOutcome::ValidatedExecutableDag { dag, .. }
        | ComposeOutcome::DraftDag { dag, .. } => dag,
        ComposeOutcome::PartialDag { dag, .. } if !dag.nodes.is_empty() => dag,
        other => panic!("[{modality}] v4 planner produced non-DAG outcome: {other:?}"),
    }
}

/// Set of node ids in a `WorkflowDag`.
fn node_ids(dag: &WorkflowDag) -> BTreeSet<String> {
    dag.nodes.iter().map(|n| n.id.clone()).collect()
}

/// Bulk-RNA-seq: `differential_expression` must have its
/// `validate_differential_expression` companion synthesized.
#[test]
fn v4_dispatch_synthesizes_validate_differential_expression() {
    let dag = run_v4_planner("bulk_rnaseq", &bulk_rnaseq_de_goal());
    let ids = node_ids(&dag);
    assert!(
        ids.contains("differential_expression"),
        "bulk-rnaseq DAG missing differential_expression node; got {ids:?}"
    );
    assert!(
        ids.contains("validate_differential_expression"),
        "bulk-rnaseq DAG missing validate_differential_expression companion; got {ids:?}"
    );
}

/// Single-cell RNA-seq: a result-producing operation atom (whichever
/// the planner reaches — clustering, dimensionality_reduction, or
/// integration) must have its `validate_*` companion. Today the
/// scrnaseq direct-path emission surfaces clustering /
/// dimensionality_reduction / integration; we assert the strongest
/// available pair.
#[test]
fn v4_dispatch_synthesizes_validate_clustering() {
    let dag = run_v4_planner("single_cell_rnaseq", &scrnaseq_clustering_goal());
    let ids = node_ids(&dag);
    // Find the strongest result-producing operation atom present in
    // this alternative; assert its companion. The fall-back order
    // matches the canonical scrnaseq pipeline shape.
    let candidates = [
        "clustering",
        "dimensionality_reduction",
        "integration",
        "normalisation",
    ];
    let target = candidates
        .iter()
        .find(|c| ids.contains(**c))
        .copied()
        .unwrap_or_else(|| {
            panic!(
                "scrnaseq DAG contains none of the canonical result-producing operation \
                 atoms ({candidates:?}); got {ids:?}"
            )
        });
    let validator = format!("validate_{target}");
    assert!(
        ids.contains(&validator),
        "scrnaseq DAG missing {validator} companion (target operation: {target}); got {ids:?}"
    );
}

/// Variant-calling: `variant_calling` must have its
/// `validate_variant_calling` companion. We also accept
/// `variant_filtering` / `variant_annotation` as fallback targets if
/// the planner's particular alternative didn't surface
/// `variant_calling` directly (the VCF chain is symmetric in port
/// shape so the planner sometimes reaches `variant_filtering` first).
#[test]
fn v4_dispatch_synthesizes_validate_variant_calling() {
    let dag = run_v4_planner("variant_calling", &variant_calling_goal());
    let ids = node_ids(&dag);
    let candidates = ["variant_calling", "variant_filtering", "variant_annotation"];
    let target = candidates
        .iter()
        .find(|c| ids.contains(**c))
        .copied()
        .unwrap_or_else(|| {
            panic!(
                "variant-calling DAG contains none of the canonical VCF-producing atoms \
                 ({candidates:?}); got {ids:?}"
            )
        });
    let validator = format!("validate_{target}");
    assert!(
        ids.contains(&validator),
        "variant-calling DAG missing {validator} companion (target operation: {target}); \
         got {ids:?}"
    );
}

/// The synthesized validator must consume its target via a
/// `from=<target>, to=validate_<target>` edge. Without the edge the
/// validator is an orphan and would block on its (empty) inputs.
#[test]
fn v4_dispatch_validate_companion_carries_an_edge_from_target() {
    let dag = run_v4_planner("bulk_rnaseq", &bulk_rnaseq_de_goal());
    let ids = node_ids(&dag);
    if !ids.contains("validate_differential_expression") {
        panic!(
            "validate_differential_expression missing; the upstream test should have caught this"
        );
    }
    let has_edge = dag.edges.iter().any(|e| {
        e.from_node == "differential_expression" && e.to_node == "validate_differential_expression"
    });
    assert!(
        has_edge,
        "synthesized validate_differential_expression node has no incoming edge from \
         differential_expression; edges={:?}",
        dag.edges
            .iter()
            .map(|e| (e.from_node.as_str(), e.to_node.as_str()))
            .collect::<Vec<_>>()
    );
}

/// Validator nodes themselves must NOT receive a companion (no
/// `validate_validate_*` recursion). The post-pass must be
/// idempotent.
#[test]
fn v4_dispatch_does_not_synthesize_companions_for_validators() {
    let dag = run_v4_planner("bulk_rnaseq", &bulk_rnaseq_de_goal());
    let ids = node_ids(&dag);
    for id in &ids {
        if id.starts_with("validate_") {
            let double = format!("validate_{id}");
            assert!(
                !ids.contains(&double),
                "found double-validate {double:?} in DAG — Task D post-pass is not \
                 idempotent. Full id set: {ids:?}"
            );
        }
    }
}
