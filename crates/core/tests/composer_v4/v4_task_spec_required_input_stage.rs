//! Regression test: the v4 composer must stamp `required_input_stage` onto the
//! data-staging anchor (`data_acquisition` / `data_import`) and thread it into
//! the lowered `Task.spec` so the executing agent knows WHICH input stage the
//! composed DAG expects to be fetched.
//!
//! Root cause this guards: `data_acquisition` declares raw_reads (data:2044) as
//! its primary output but received NO signal about the composed DAG's entry
//! point, so the agent could fetch whatever was easiest (a deposited
//! supplementary count matrix) even when the DAG needed raw reads — a silent
//! stall. `composer_v4::planner::stamp_required_input_stage` now resolves the
//! entry point from `goal.modifiers["available_input_stage"]` (default
//! data:2044) and the WORKFLOW.json lowering pass folds it into task-spec.json.

use ecaa_workflow_core::archetype_registry::ArchetypeRegistry;
use ecaa_workflow_core::atom_registry::AtomRegistry;
use ecaa_workflow_core::builder::build_dag_from_workflow_dag;
use ecaa_workflow_core::composer::compose_with_modalities_full;
use ecaa_workflow_core::goal_spec::GoalSpec;
use std::collections::BTreeMap;
use std::path::Path;

fn load_registries() -> (AtomRegistry, ArchetypeRegistry) {
    let atoms = AtomRegistry::load_from_dir(Path::new("../../config/stage-atoms"))
        .expect("load stage-atoms registry");
    let archetypes = ArchetypeRegistry::load_from_dir(Path::new("../../config/archetypes"))
        .expect("load archetypes registry");
    (atoms, archetypes)
}

/// The staging anchor's lowered `required_input_stage` for a compose call,
/// resolved by finding the task whose source atom is `data_acquisition` /
/// `data_import`. Panics if no anchor task exists (that itself is a defect).
fn anchor_required_input_stage(
    goal: &GoalSpec,
    modalities: &[&str],
    atoms: &AtomRegistry,
    archetypes: &ArchetypeRegistry,
) -> String {
    let out = compose_with_modalities_full(
        goal,
        "bioinformatics",
        atoms,
        archetypes,
        modalities,
        None,
        None,
        None,
    )
    .expect("v4 compose");
    let wf = out.workflow_dag.as_ref().expect("v4 workflow_dag");
    let dag = build_dag_from_workflow_dag(wf, "wf-test").expect("lower v4 dag");

    let mut found: Option<String> = None;
    for (task_id, task) in &dag.tasks {
        let is_anchor = matches!(
            task.source_atom_id.as_deref(),
            Some("data_acquisition") | Some("data_import")
        );
        if !is_anchor {
            continue;
        }
        let ris = task
            .spec
            .as_ref()
            .and_then(|s| s.get("required_input_stage"))
            .and_then(|v| v.as_str())
            .unwrap_or_else(|| {
                panic!(
                    "{task_id}: staging anchor must carry spec.required_input_stage; got {:?}",
                    task.spec
                )
            });
        found = Some(ris.to_string());
    }
    found.expect("compose must produce a data_acquisition / data_import anchor task")
}

/// Raw-default DAG: a bulk RNA-seq DE pipeline with NO supplied-product signal
/// must stamp the raw-reads entry point (`data:2044`) so the agent fetches
/// FASTQs rather than substituting a deposited processed matrix.
#[test]
fn raw_default_dag_stamps_data_2044() {
    let (atoms, archetypes) = load_registries();
    let mut modifiers = BTreeMap::new();
    modifiers.insert("kind".to_string(), "differential_expression".to_string());
    let goal = GoalSpec {
        edam_data: "data:0951".into(),
        edam_format: Some("format:3475".into()),
        modifiers,
        source_prose: Some("differential expression".into()),
        confidence: 1.0,
    };
    let iri = anchor_required_input_stage(&goal, &["bulk_rnaseq"], &atoms, &archetypes);
    assert_eq!(
        iri, "data:2044",
        "a raw-default DAG must stamp the raw-reads entry point on the staging anchor"
    );
}

/// Counts-first / downstream DAG: when the classifier stamped
/// `available_input_stage = data:3917` (SME supplied a prepared counts matrix),
/// the staging anchor must carry THAT entry point so the agent materializes the
/// supplied counts product instead of re-fetching raw reads.
#[test]
fn counts_first_dag_stamps_data_3917() {
    let (atoms, archetypes) = load_registries();
    let mut modifiers = BTreeMap::new();
    modifiers.insert("kind".to_string(), "differential_expression".to_string());
    modifiers.insert("available_input_stage".to_string(), "data:3917".to_string());
    let goal = GoalSpec {
        edam_data: "data:0951".into(),
        edam_format: Some("format:3475".into()),
        modifiers,
        source_prose: Some(
            "differential expression, counts matrix already prepared, no raw FASTQs".into(),
        ),
        confidence: 1.0,
    };
    let iri = anchor_required_input_stage(&goal, &["bulk_rnaseq"], &atoms, &archetypes);
    assert_eq!(
        iri, "data:3917",
        "a counts-first DAG must stamp the supplied processed-product entry point"
    );
}
