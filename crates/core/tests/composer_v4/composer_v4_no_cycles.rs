//! v4 emissions must not contain cycles.
//!
//! The meet-in-middle path could otherwise construct edges purely
//! from port type compatibility. When two atoms both consume and
//! produce the same `data:*` type (e.g., `batch_correction` and
//! `differential_expression` both touch `data:3917` count matrix),
//! the search would emit edges in both directions, producing a cycle
//! that `validate_dag` rejects.
//!
//! The planner uses the atom registry's `depends_on` field to compute
//! a topological rank for each atom, then rejects candidate edges
//! where producer's rank exceeds consumer's rank. This is the "the
//! YAML knows the canonical workflow order" approach — no separate
//! role table to maintain.
//!
//! The three flagship scenarios from the parity corpus exercise the
//! representative cycle shapes:
//!
//! - **bulk-rnaseq** — count-matrix-consuming atoms produce
//! count matrices (`batch_correction` ↔ `differential_expression`)
//! - **scrnaseq** — same, plus the integration / clustering chain
//! - **variant-calling** — VCF-shaped atoms
//! (`variant_calling` → `variant_filtering` → `variant_annotation`)
//! all produce VCFs that match each others' input ports
//!
//! The tests run the v4 planner end-to-end through the same
//! `composer_v4::plan` entry point the parity corpus uses, then lower
//! the resulting WorkflowDag and assert the lowered `DAG` passes
//! `validate_dag`. We deliberately bypass the production-path
//! `compose_v4_dispatch_full` wrapper (which surfaces orthogonal
//! `MethodChoiceUnresolved` failures from `validate_composition` —
//! those are Task C territory). Task B's job is "the planner doesn't
//! produce cycles"; the production-path validation is wrapped on top.

use std::collections::BTreeMap;
use std::path::Path;

use ecaa_workflow_core::archetype_registry::ArchetypeRegistry;
use ecaa_workflow_core::atom_registry::AtomRegistry;
use ecaa_workflow_core::builder::build_dag_from_workflow_dag;
use ecaa_workflow_core::composer_v4::{plan as v4_plan, PlanningContext};
use ecaa_workflow_core::dag::{validate_dag, DAG};
use ecaa_workflow_core::goal_spec::GoalSpec;
use ecaa_workflow_core::workflow_contracts::data_product::DataProductContract;
use ecaa_workflow_core::workflow_contracts::outcome::ComposeOutcome;
use ecaa_workflow_core::workflow_contracts::workflow_intent::{DesiredOutput, WorkflowIntent};

const ATOMS_DIR: &str = "../../config/stage-atoms";
const ARCHETYPES_DIR: &str = "../../config/archetypes";

/// Bulk-RNA-seq DE goal — IBD cohort, salmon quant, DESeq2.
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

/// Single-cell RNA-seq clustering goal.
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

/// Germline variant calling goal.
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

/// Run the v4 planner directly with a hand-crafted intent and lower
/// the resulting WorkflowDag. Mirrors the parity-corpus regenerator's
/// direct-path strategy. The production dispatch wrapper layers
/// extra validation (method_choice, exclusion-consistency) that is
/// orthogonal to cycle prevention; Task B's regression sits one
/// level deeper at the planner output.
fn run_v4_planner(modality: &str, goal: &GoalSpec) -> Result<DAG, String> {
    let atom_reg = AtomRegistry::load_from_dir(Path::new(ATOMS_DIR))
        .expect("AtomRegistry must load from config/stage-atoms");
    let archetype_reg = ArchetypeRegistry::load_from_dir(Path::new(ARCHETYPES_DIR))
        .expect("ArchetypeRegistry must load from config/archetypes");
    let intent = WorkflowIntent {
        id: format!("v4_no_cycles_{modality}"),
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
    let workflow_dag = match &result.primary {
        ComposeOutcome::ValidatedExecutableDag { dag, .. }
        | ComposeOutcome::DraftDag { dag, .. } => dag.clone(),
        ComposeOutcome::PartialDag {
            dag,
            unresolved_gaps,
        } if !dag.nodes.is_empty() => {
            // PartialDag with content is acceptable for the cycle
            // regression — Task B only cares about edge structure.
            // The remaining gaps are downstream concerns.
            eprintln!(
                "[{modality}] PartialDag with {} gap(s); using DAG anyway",
                unresolved_gaps.len()
            );
            dag.clone()
        }
        other => return Err(format!("planner produced non-DAG outcome: {other:?}")),
    };
    build_dag_from_workflow_dag(&workflow_dag, &format!("v4_no_cycles_{modality}"))
        .map_err(|e| format!("build_dag_from_workflow_dag: {e:?}"))
}

/// Bulk-RNA-seq is the canonical regression: pre-fix the dispatch
/// returned a `CycleDetected` carrying batch_correction +
/// sequence_trimming + alignment + clustering + integration among
/// others (count-matrix-shaped atoms feeding each other).
#[test]
fn v4_emissions_have_no_cycles_bulk_rnaseq() {
    let dag = run_v4_planner("bulk_rnaseq", &bulk_rnaseq_de_goal())
        .unwrap_or_else(|e| panic!("bulk-rnaseq v4 planner failed: {e}"));
    let validate_result = validate_dag(&dag);
    assert!(
        validate_result.is_ok(),
        "bulk-rnaseq v4 DAG fails validate_dag: {:?}\n  task ids: {:?}",
        validate_result,
        dag.tasks.keys().collect::<Vec<_>>()
    );
}

/// Single-cell exercises the same count-matrix cycle pattern plus
/// the clustering/integration follow-on.
#[test]
fn v4_emissions_have_no_cycles_scrnaseq() {
    let dag = run_v4_planner("single_cell_rnaseq", &scrnaseq_clustering_goal())
        .unwrap_or_else(|e| panic!("scrnaseq v4 planner failed: {e}"));
    let validate_result = validate_dag(&dag);
    assert!(
        validate_result.is_ok(),
        "scrnaseq v4 DAG fails validate_dag: {:?}\n  task ids: {:?}",
        validate_result,
        dag.tasks.keys().collect::<Vec<_>>()
    );
}

/// Variant calling exercises the VCF-passing chain
/// (variant_calling → variant_filtering → variant_annotation): each
/// produces VCFs that satisfy the others' input port shapes.
#[test]
fn v4_emissions_have_no_cycles_variant_calling() {
    let dag = run_v4_planner("variant_calling", &variant_calling_goal())
        .unwrap_or_else(|e| panic!("variant-calling v4 planner failed: {e}"));
    let validate_result = validate_dag(&dag);
    assert!(
        validate_result.is_ok(),
        "variant-calling v4 DAG fails validate_dag: {:?}\n  task ids: {:?}",
        validate_result,
        dag.tasks.keys().collect::<Vec<_>>()
    );
}
