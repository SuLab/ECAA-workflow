//! The crispr_repair_scar archetype must compose to an acyclic DAG whose
//! primary chain contains the four new atoms and terminates in final_reporting.

use std::collections::BTreeMap;
use std::path::Path;

use ecaa_workflow_core::archetype_registry::ArchetypeRegistry;
use ecaa_workflow_core::atom_registry::AtomRegistry;
use ecaa_workflow_core::builder::build_dag_from_workflow_dag;
use ecaa_workflow_core::composer_v4::{plan as v4_plan, PlanningContext};
use ecaa_workflow_core::dag::{validate_dag, DAG};
use ecaa_workflow_core::goal_spec::GoalSpec;
use ecaa_workflow_core::ids::TaskId;
use ecaa_workflow_core::workflow_contracts::data_product::DataProductContract;
use ecaa_workflow_core::workflow_contracts::outcome::ComposeOutcome;
use ecaa_workflow_core::workflow_contracts::workflow_intent::{DesiredOutput, WorkflowIntent};

const ATOMS_DIR: &str = "../../config/stage-atoms";
const ARCHETYPES_DIR: &str = "../../config/archetypes";

fn repair_scar_goal() -> GoalSpec {
    GoalSpec {
        edam_data: "ecaax:repair_scar_table".into(),
        edam_format: Some("format:3475".into()),
        modifiers: BTreeMap::new(),
        source_prose: Some(
            "PacBio HiFi amplicon CRISPR repair-scar characterization: demultiplex by \
             barcode, structural read gate, remove human-identical reads, pbmm2 align \
             to the construct, analyze the repair scar per read."
                .into(),
        ),
        confidence: 0.9,
    }
}

fn run_planner() -> Result<DAG, String> {
    let atom_reg = AtomRegistry::load_from_dir(Path::new(ATOMS_DIR)).expect("atoms load");
    let archetype_reg =
        ArchetypeRegistry::load_from_dir(Path::new(ARCHETYPES_DIR)).expect("archetypes load");
    let goal = repair_scar_goal();
    let intent = WorkflowIntent {
        id: "v4_repair_scar".into(),
        schema_version: semver::Version::new(1, 0, 0),
        goal: goal.source_prose.clone().unwrap(),
        modality: Some("crispr_amplicon_editing".into()),
        project_class: Some("bioinformatics".into()),
        available_data: vec![DataProductContract::sample_paired_fastq()],
        desired_outputs: vec![DesiredOutput {
            label: "repair scar table".into(),
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

    let result = v4_plan(&ctx, &goal, "bioinformatics", &atom_reg, &archetype_reg);
    let workflow_dag = match &result.primary {
        ComposeOutcome::ValidatedExecutableDag { dag, .. }
        | ComposeOutcome::DraftDag { dag, .. } => dag.clone(),
        ComposeOutcome::PartialDag { dag, .. } if !dag.nodes.is_empty() => dag.clone(),
        other => return Err(format!("non-DAG outcome: {other:?}")),
    };
    build_dag_from_workflow_dag(&workflow_dag, "v4_repair_scar")
        .map_err(|e| format!("lower: {e:?}"))
}

#[test]
fn repair_scar_archetype_composes_acyclic_dag() {
    let dag = run_planner().unwrap_or_else(|e| panic!("planner failed: {e}"));
    assert!(
        validate_dag(&dag).is_ok(),
        "repair-scar DAG fails validate_dag: {:?}\n  tasks: {:?}",
        validate_dag(&dag),
        dag.tasks.keys().collect::<Vec<_>>()
    );
    let ids: Vec<&TaskId> = dag.tasks.keys().collect();
    for expected in [
        "demultiplex_barcodes",
        "filter_reads_by_structure",
        "filter_host_contamination",
        "repair_scar_analysis",
        "final_reporting",
    ] {
        assert!(
            ids.iter().any(|k| k.as_str() == expected),
            "expected task {expected:?} in composed DAG; got {ids:?}"
        );
    }
}
