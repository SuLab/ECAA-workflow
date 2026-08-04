//! The v4 lowering path must convert each compute task's declared figure
//! contract into machine-checked `Task.required_artifacts`, not just into
//! the advisory `Task.spec.required_figures` the executing agent reads.
//!
//! Without this the harness's silent-completion guard had nothing to
//! check on the only live composition path: `required_artifacts` was
//! populated solely from the handful of atoms that declare
//! `required_artifacts:` literally in YAML, so a compute task could
//! report `completed` having rendered none of its plots and the
//! completion would stand.
//!
//! Scope contract asserted here, mirroring the stage-driven path's
//! `required_artifacts_for_stage(stage, include_figures = true)`:
//! - compute tasks with a non-empty figure contract require
//!   `figures/manifest.json` plus one `figures/<id>.png` per figure;
//! - the PDF sibling is deliberately NOT required (theme-configurable
//!   output format, so gating on it would block runs that produced every
//!   figure);
//! - validate/discover/gate/review companions acquire no figure
//!   obligations, because the plots land in the parent compute task's
//!   output directory, not theirs;
//! - atom-declared `required_artifacts` survive alongside the
//!   synthesized figure entries.

use ecaa_workflow_core::archetype_registry::ArchetypeRegistry;
use ecaa_workflow_core::atom_registry::AtomRegistry;
use ecaa_workflow_core::builder::build_dag_from_workflow_dag;
use ecaa_workflow_core::composer::compose_with_modalities_full;
use ecaa_workflow_core::dag::{TaskKind, DAG};
use ecaa_workflow_core::goal_spec::GoalSpec;
use std::collections::BTreeMap;
use std::path::Path;

fn bulk_rnaseq_de_dag() -> DAG {
    let atoms = AtomRegistry::load_from_dir(Path::new("../../config/stage-atoms"))
        .expect("load stage-atoms registry");
    let archetypes = ArchetypeRegistry::load_from_dir(Path::new("../../config/archetypes"))
        .expect("load archetypes registry");

    let mut modifiers = BTreeMap::new();
    modifiers.insert("kind".to_string(), "differential_expression".to_string());
    let goal = GoalSpec {
        edam_data: "data:0951".into(),
        edam_format: Some("format:3475".into()),
        modifiers,
        source_prose: Some("differential expression".into()),
        confidence: 1.0,
    };

    let out = compose_with_modalities_full(
        &goal,
        "bioinformatics",
        &atoms,
        &archetypes,
        &["bulk_rnaseq"],
        None,
        None,
        None,
    )
    .expect("v4 compose");

    let wf = out.workflow_dag.as_ref().expect("v4 workflow_dag");
    build_dag_from_workflow_dag(wf, "wf-test").expect("lower v4 dag")
}

fn declared_figures(task: &ecaa_workflow_core::dag::Task) -> Vec<String> {
    task.spec
        .as_ref()
        .and_then(|s| s.get("required_figures"))
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

#[test]
fn v4_compute_tasks_require_their_declared_figures_as_artifacts() {
    let dag = bulk_rnaseq_de_dag();

    let mut violations: Vec<String> = Vec::new();
    let mut checked_compute = 0usize;

    for (task_id, task) in &dag.tasks {
        let figures = declared_figures(task);
        let paths: Vec<&str> = task
            .required_artifacts
            .iter()
            .map(|a| a.path.as_str())
            .collect();

        if !matches!(task.kind, TaskKind::Computation) {
            // Non-compute companions must acquire no figure obligation
            // regardless of what their spec carries.
            if let Some(bad) = paths.iter().find(|p| p.starts_with("figures/")) {
                violations.push(format!(
                    "{task_id}: {:?} task must not carry figure artifact {bad}",
                    task.kind
                ));
            }
            continue;
        }

        if figures.is_empty() {
            if let Some(bad) = paths.iter().find(|p| p.starts_with("figures/")) {
                violations.push(format!(
                    "{task_id}: no declared figures but carries figure artifact {bad}"
                ));
            }
            continue;
        }

        checked_compute += 1;

        if !paths.contains(&"figures/manifest.json") {
            violations.push(format!(
                "{task_id}: declares {figures:?} but required_artifacts lack figures/manifest.json ({paths:?})"
            ));
        }
        for fig in &figures {
            let want = format!("figures/{fig}.png");
            if !paths.iter().any(|p| *p == want.as_str()) {
                violations.push(format!(
                    "{task_id}: declares figure {fig} but required_artifacts lack {want} ({paths:?})"
                ));
            }
        }
        // The PDF sibling is a theme-configurable output format; requiring
        // it would let a theme change re-block a complete run.
        if let Some(bad) = paths
            .iter()
            .find(|p| p.starts_with("figures/") && p.ends_with(".pdf"))
        {
            violations.push(format!(
                "{task_id}: PDF sibling {bad} must not be a hard obligation"
            ));
        }
        // Every figure obligation is a presence gate; table-oriented
        // validation runners bound to a PNG only soft-skip.
        for artifact in &task.required_artifacts {
            if artifact.path.starts_with("figures/") && !artifact.validation_obligations.is_empty()
            {
                violations.push(format!(
                    "{task_id}: figure artifact {} must carry no validation_obligations, got {:?}",
                    artifact.path, artifact.validation_obligations
                ));
            }
            if artifact.path.starts_with("figures/") && artifact.min_size_bytes.is_none() {
                violations.push(format!(
                    "{task_id}: figure artifact {} must declare a non-empty min_size_bytes",
                    artifact.path
                ));
            }
        }
    }

    assert!(
        checked_compute >= 3,
        "test harness drift: expected several compute tasks with a figure contract, checked {checked_compute}"
    );
    assert!(
        violations.is_empty(),
        "{} figure-obligation violations:\n{}",
        violations.len(),
        violations.join("\n")
    );
}

#[test]
fn v4_figure_obligations_do_not_displace_atom_declared_artifacts() {
    let dag = bulk_rnaseq_de_dag();

    // `pathway_enrichment` is the one bulk_rnaseq stage whose atom
    // declares BOTH a literal required artifact and a figure, so it is
    // the merge case: the synthesized figure entries must be appended,
    // never substituted for the atom's own declaration.
    let task = dag
        .tasks
        .get("pathway_enrichment")
        .expect("bulk_rnaseq DE DAG must contain pathway_enrichment");
    let paths: Vec<&str> = task
        .required_artifacts
        .iter()
        .map(|a| a.path.as_str())
        .collect();
    assert!(
        paths.contains(&"ranked_genes.tsv"),
        "atom-declared artifact dropped: {paths:?}"
    );
    assert!(
        paths.contains(&"figures/manifest.json"),
        "figure manifest obligation missing: {paths:?}"
    );
    assert!(
        paths.contains(&"figures/top_enriched_terms.png"),
        "figure obligation missing: {paths:?}"
    );
    // The atom's own artifact keeps the shape the lowering pass gave it —
    // the figure pass appends, it never rewrites an existing entry.
    let ranked = task
        .required_artifacts
        .iter()
        .find(|a| a.path == "ranked_genes.tsv")
        .expect("ranked_genes.tsv present");
    assert_eq!(
        ranked.min_size_bytes,
        Some(1),
        "atom-declared artifact was rewritten by the figure pass"
    );

    // A literature stage carries atom-declared artifacts WITH validation
    // obligations and no figure contract; nothing about it may change.
    let survey = dag
        .tasks
        .get("survey_method_landscape")
        .expect("bulk_rnaseq DE DAG must contain survey_method_landscape");
    let with_obligations = survey
        .required_artifacts
        .iter()
        .filter(|a| !a.validation_obligations.is_empty())
        .count();
    assert!(
        with_obligations > 0,
        "expected atom-declared artifacts carrying validation obligations, got {:?}",
        survey.required_artifacts
    );
    assert!(
        !survey
            .required_artifacts
            .iter()
            .any(|a| a.path.starts_with("figures/")),
        "stage with no figure contract gained a figure obligation: {:?}",
        survey.required_artifacts
    );
}

#[test]
fn v4_figure_obligations_are_deterministic_across_builds() {
    let first = bulk_rnaseq_de_dag();
    let second = bulk_rnaseq_de_dag();

    let extract = |dag: &DAG| -> Vec<(String, Vec<String>)> {
        dag.tasks
            .iter()
            .map(|(id, t)| {
                (
                    id.to_string(),
                    t.required_artifacts
                        .iter()
                        .map(|a| a.path.clone())
                        .collect(),
                )
            })
            .collect()
    };

    assert_eq!(
        extract(&first),
        extract(&second),
        "required_artifacts ordering must be byte-stable across builds"
    );
}
