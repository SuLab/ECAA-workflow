//! Regression test: the v4 composer's CLI intake path must populate
//! `Task.spec.required_figures` + `Task.source_atom_id` for every
//! lowered task whose source atom declares those fields.
//!
//! Bug: `lower_to_workflow_json::lower_task` was producing
//! `Task { spec: None }` for every task even when the
//! WorkflowDag node's attributes carried `required_figures`,
//! `plot_stage_id`, `expected_artifacts`. Agents read
//! `runtime/outputs/<task_id>/task-spec.json` to find their
//! figures contract; with `spec: null` they had nothing to honour.

use scripps_workflow_core::archetype_registry::ArchetypeRegistry;
use scripps_workflow_core::atom_registry::AtomRegistry;
use scripps_workflow_core::builder::build_dag_from_workflow_dag;
use scripps_workflow_core::composer::compose_with_version_and_modalities_full;
use scripps_workflow_core::goal_spec::GoalSpec;
use std::collections::BTreeMap;
use std::path::Path;

#[test]
fn v4_bulk_rnaseq_de_threads_required_figures_into_task_spec() {
    let atoms = AtomRegistry::load_from_dir(Path::new("../../config/stage-atoms"))
        .expect("load stage-atoms registry");
    let archetypes = ArchetypeRegistry::load_from_dir(Path::new("../../config/archetypes"))
        .expect("load archetypes registry");

    // Use the bulk_rnaseq_de archetype's canonical goal — same
    // shape the classifier produces for the IBD scenario.
    let mut modifiers = BTreeMap::new();
    modifiers.insert("kind".to_string(), "differential_expression".to_string());
    let goal = GoalSpec {
        edam_data: "data:0951".into(),
        edam_format: Some("format:3475".into()),
        modifiers,
        source_prose: Some("differential expression".into()),
        confidence: 1.0,
    };
    let modalities = vec!["bulk_rnaseq"];

    let out = compose_with_version_and_modalities_full(
        &goal,
        "bioinformatics",
        &atoms,
        &archetypes,
        4,
        &modalities,
        None,
        None,
        None,
    )
    .expect("v4 compose");

    let wf = out.workflow_dag.as_ref().expect("v4 workflow_dag");
    let dag = build_dag_from_workflow_dag(wf, "wf-test").expect("lower v4 dag");

    // Every task whose source atom declares required_figures must
    // surface them in Task.spec.required_figures.
    let mut violations: Vec<String> = Vec::new();
    let mut checked_figures = 0;
    let mut checked_atom_id = 0;
    for (task_id, task) in &dag.tasks {
        let id_str = task_id.as_str();
        // Skip synthesized validate_ and discover_ companions — they
        // don't have an authored atom with required_figures.
        if id_str.starts_with("validate_") || id_str.starts_with("discover_") {
            continue;
        }
        let Some(atom) = atoms.get(id_str) else {
            continue;
        };
        if !atom.required_figures.is_empty() {
            checked_figures += 1;
            let figures = task
                .spec
                .as_ref()
                .and_then(|s| s.get("required_figures"))
                .and_then(|v| v.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            if figures.is_empty() {
                violations.push(format!(
                    "{}: atom declares {:?} required_figures but Task.spec is {:?}",
                    id_str, atom.required_figures, task.spec
                ));
            }
        }
        // Non-validate / non-discover tasks must carry source_atom_id.
        if task.source_atom_id.is_none() {
            violations.push(format!("{}: source_atom_id is None", id_str));
        } else {
            checked_atom_id += 1;
        }
    }

    assert!(
        checked_figures > 0,
        "test harness drift: no atoms with required_figures were checked"
    );
    assert!(
        checked_atom_id > 0,
        "test harness drift: no non-validate/non-discover tasks checked for source_atom_id"
    );
    assert!(
        violations.is_empty(),
        "{} tasks failed Task.spec threading:\n{}",
        violations.len(),
        violations.join("\n")
    );
}
