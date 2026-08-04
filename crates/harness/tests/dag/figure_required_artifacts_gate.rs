//! Both directions of the figure-artifact gate, evaluated with the real
//! harness verifier against a real composed DAG.
//!
//! The composer now converts a compute task's declared figure contract
//! into `Task.required_artifacts`. That is only safe if it holds in BOTH
//! directions: a task that produced its declared figures must NOT be
//! flagged, and a task that omitted one MUST be. A one-directional
//! change here would re-block correct runs, which is the failure mode
//! the guard exists to avoid, not to create.
//!
//! `verify_required_artifacts` is the exact function the harness's
//! silent-completion guard calls before letting a `Completed` task stand
//! (a non-empty return becomes a `[missing_artifact]` re-block that the
//! server promotes to `BlockerKind::MissingArtifact`).

use ecaa_workflow_core::archetype_registry::ArchetypeRegistry;
use ecaa_workflow_core::atom_registry::AtomRegistry;
use ecaa_workflow_core::builder::build_dag_from_workflow_dag;
use ecaa_workflow_core::composer::compose_with_modalities_full;
use ecaa_workflow_core::dag::{RequiredArtifact, TaskKind};
use ecaa_workflow_core::goal_spec::GoalSpec;
use ecaa_workflow_harness::required_artifacts::verify_required_artifacts;
use std::collections::BTreeMap;
use std::path::Path;

/// Compose the bulk RNA-seq DE workflow and return the first compute task
/// (id + required artifacts) that carries figure obligations.
fn compute_task_with_figure_obligations() -> (String, Vec<RequiredArtifact>) {
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
    let dag = build_dag_from_workflow_dag(wf, "wf-test").expect("lower v4 dag");

    dag.tasks
        .iter()
        .find(|(_, t)| {
            matches!(t.kind, TaskKind::Computation)
                && t.required_artifacts
                    .iter()
                    .any(|a| a.path.starts_with("figures/") && a.path.ends_with(".png"))
        })
        .map(|(id, t)| (id.to_string(), t.required_artifacts.clone()))
        .expect("at least one compute task must carry figure obligations")
}

/// Write every declared artifact as a non-empty file under the task's
/// output dir, so the task looks like a correct, complete run.
fn materialize(root: &Path, task_id: &str, artifacts: &[RequiredArtifact]) {
    let base = root.join("runtime/outputs").join(task_id);
    for a in artifacts {
        let full = base.join(&a.path);
        std::fs::create_dir_all(full.parent().expect("artifact has a parent dir"))
            .expect("create artifact dir");
        // Satisfy the declared minimum with a byte pattern, not an empty
        // file — `verify_required_artifacts` treats size 0 as missing.
        let min = a.min_size_bytes.unwrap_or(1).max(1) as usize;
        std::fs::write(&full, "x".repeat(min)).expect("write artifact");
    }
}

#[test]
fn a_task_that_produced_its_declared_figures_is_not_flagged() {
    let (task_id, artifacts) = compute_task_with_figure_obligations();
    let tmp = tempfile::tempdir().expect("tempdir");
    materialize(tmp.path(), &task_id, &artifacts);

    let missing =
        verify_required_artifacts(tmp.path(), &task_id, &artifacts).expect("verify succeeds");
    assert!(
        missing.is_empty(),
        "complete run must not be flagged; missing={missing:?}"
    );
}

#[test]
fn a_task_that_omitted_one_declared_figure_is_flagged() {
    let (task_id, artifacts) = compute_task_with_figure_obligations();
    let omitted = artifacts
        .iter()
        .find(|a| a.path.starts_with("figures/") && a.path.ends_with(".png"))
        .expect("a figure obligation to omit")
        .path
        .clone();

    let tmp = tempfile::tempdir().expect("tempdir");
    materialize(tmp.path(), &task_id, &artifacts);
    std::fs::remove_file(
        tmp.path()
            .join("runtime/outputs")
            .join(&task_id)
            .join(&omitted),
    )
    .expect("remove the omitted figure");

    let missing =
        verify_required_artifacts(tmp.path(), &task_id, &artifacts).expect("verify succeeds");
    assert_eq!(
        missing,
        vec![omitted.clone()],
        "omitting {omitted} must be the one and only flagged artifact"
    );
}

#[test]
fn a_zero_byte_figure_is_flagged_as_missing() {
    let (task_id, artifacts) = compute_task_with_figure_obligations();
    let truncated = artifacts
        .iter()
        .find(|a| a.path.starts_with("figures/") && a.path.ends_with(".png"))
        .expect("a figure obligation to truncate")
        .path
        .clone();

    let tmp = tempfile::tempdir().expect("tempdir");
    materialize(tmp.path(), &task_id, &artifacts);
    std::fs::write(
        tmp.path()
            .join("runtime/outputs")
            .join(&task_id)
            .join(&truncated),
        [],
    )
    .expect("truncate the figure");

    let missing =
        verify_required_artifacts(tmp.path(), &task_id, &artifacts).expect("verify succeeds");
    assert_eq!(
        missing,
        vec![truncated.clone()],
        "a zero-byte {truncated} must be flagged"
    );
}

#[test]
fn a_missing_figure_manifest_is_flagged() {
    let (task_id, artifacts) = compute_task_with_figure_obligations();
    assert!(
        artifacts.iter().any(|a| a.path == "figures/manifest.json"),
        "figure contract must include the render manifest; got {:?}",
        artifacts.iter().map(|a| &a.path).collect::<Vec<_>>()
    );

    let tmp = tempfile::tempdir().expect("tempdir");
    materialize(tmp.path(), &task_id, &artifacts);
    std::fs::remove_file(
        tmp.path()
            .join("runtime/outputs")
            .join(&task_id)
            .join("figures/manifest.json"),
    )
    .expect("remove the manifest");

    let missing =
        verify_required_artifacts(tmp.path(), &task_id, &artifacts).expect("verify succeeds");
    assert_eq!(
        missing,
        vec!["figures/manifest.json".to_string()],
        "a render step that never ran must be flagged"
    );
}
