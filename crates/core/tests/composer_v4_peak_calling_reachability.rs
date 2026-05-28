//! `peak_calling` must be reachable from chip-seq + atac-seq goals
//! via v4 dispatch.
//!
//! The goal pattern in `config/modality-keywords.yaml` declares
//! `data:1255` (Feature record / annotation-track) for the
//! peak-calling phrase set, matching `peak_calling.yaml`'s output
//! port. Both chip-seq and atac-seq forward + backward search reach
//! `peak_calling`; the production dispatch returns a
//! `ValidatedExecutableDag` whose task set contains it. If the goal
//! IRI ever drifted to `data:0863` (BAM / Sequence alignment) again,
//! there's no subsumption edge to `data:1255` in
//! `crates/core/src/edam.rs::edam_subtype_edges`, so
//! `engine.prove(producer_output, goal)` would return
//! `Incompatible::SemanticTypeMismatch` and the backward search
//! would never enqueue `peak_calling`.
//!
//! Mirrors the helper shape in `composer_v4_atom_selection.rs` so the
//! per-Task TDD scaffolds stay close together.

use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::path::Path;

use scripps_workflow_core::archetype_registry::ArchetypeRegistry;
use scripps_workflow_core::atom_registry::AtomRegistry;
use scripps_workflow_core::composer_v4::{plan as v4_plan, PlanningContext};
use scripps_workflow_core::goal_spec::GoalSpec;
use scripps_workflow_core::workflow_contracts::data_product::DataProductContract;
use scripps_workflow_core::workflow_contracts::outcome::ComposeOutcome;
use scripps_workflow_core::workflow_contracts::workflow_intent::{DesiredOutput, WorkflowIntent};

const ATOMS_DIR: &str = "../../config/stage-atoms";
const ARCHETYPES_DIR: &str = "../../config/archetypes";

/// Goal mirrored from `config/modality-keywords.yaml`'s peak-calling
/// pattern (post-fix: `data:1255` / `format:3003`). The pattern's
/// pre-fix shape was `data:0863` / `format:3003` which is internally
/// inconsistent (BAM data type with BED physical format). The fix
/// replaces the data IRI with the correct EDAM term for a BED-format
/// peak feature-record set.
fn peak_calling_goal(prose: &str) -> GoalSpec {
    GoalSpec {
        edam_data: "data:1255".into(),
        edam_format: Some("format:3003".into()),
        modifiers: BTreeMap::from([("kind".into(), "peak_calling".into())]),
        source_prose: Some(prose.into()),
        confidence: 1.0,
    }
}

/// Run the v4 planner with the given modality + peak-calling goal,
/// then return the set of atom (task-node) ids in the primary
/// outcome's DAG. Mirrors `composer_v4_atom_selection.rs::task_ids_for`
/// so both Task C and Task F regressions share the same scaffold.
fn task_ids_for(modality: &str, goal: &GoalSpec) -> BTreeSet<String> {
    let atom_reg = AtomRegistry::load_from_dir(Path::new(ATOMS_DIR))
        .expect("AtomRegistry must load from config/stage-atoms");
    let archetype_reg = ArchetypeRegistry::load_from_dir(Path::new(ARCHETYPES_DIR))
        .expect("ArchetypeRegistry must load from config/archetypes");
    let intent = WorkflowIntent {
        id: format!("v4_peak_calling_{modality}"),
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

    let mut ids = BTreeSet::new();
    let dag_from = |outcome: &ComposeOutcome| match outcome {
        ComposeOutcome::ValidatedExecutableDag { dag, .. }
        | ComposeOutcome::DraftDag { dag, .. }
        | ComposeOutcome::PartialDag { dag, .. } => Some(dag.clone()),
        _ => None,
    };
    if let Some(dag) = dag_from(&result.primary) {
        for n in dag.nodes {
            ids.insert(n.id);
        }
    }
    for alt in &result.alternatives {
        for n in &alt.dag.nodes {
            ids.insert(n.id.clone());
        }
    }
    ids
}

/// Chip-seq scenarios must reach `peak_calling` via v4 dispatch.
/// If the goal IRI drifts back to `data:0863 (format:3003)`,
/// `compose_v4_dispatch_full` errors
/// `GoalUnreachable { goal: "data:0863 (format:3003)" }` because the
/// goal IRI doesn't unify with `peak_calling`'s output port
/// (`data:1255`).
#[test]
fn peak_calling_reachable_for_chip_seq() {
    let goal = peak_calling_goal(
        "Align reads with BWA, call narrow peaks with MACS2 against matched input controls",
    );
    let task_ids = task_ids_for("chip_seq", &goal);
    assert!(
        task_ids.contains("peak_calling"),
        "peak_calling missing from chip-seq v4 DAG: {task_ids:?}"
    );
}

/// Atac-seq scenarios must reach `peak_calling` via
/// v4 dispatch. Same root cause as chip-seq: the shared `peak_calling`
/// atom serves both modalities.
#[test]
fn peak_calling_reachable_for_atac_seq() {
    let goal = peak_calling_goal(
        "Align reads, call peaks of accessible chromatin with MACS2 in BAMPE mode",
    );
    let task_ids = task_ids_for("atac_seq", &goal);
    assert!(
        task_ids.contains("peak_calling"),
        "peak_calling missing from atac-seq v4 DAG: {task_ids:?}"
    );
}

/// `validate_peak_calling` companion must also be synthesized.
/// Asserting on the chip-seq side is sufficient; the
/// companion-synthesis pass runs identically on both modalities.
#[test]
fn validate_peak_calling_reachable_for_chip_seq() {
    let goal = peak_calling_goal(
        "Align reads with BWA, call narrow peaks with MACS2 against matched input controls",
    );
    let task_ids = task_ids_for("chip_seq", &goal);
    assert!(
        task_ids.contains("validate_peak_calling"),
        "validate_peak_calling missing from chip-seq v4 DAG: {task_ids:?}"
    );
}

/// Sanity guard that the goal-pattern fix in
/// `config/modality-keywords.yaml` matches the atom's output IRI. If
/// the YAML drifts (someone reverts the IRI to `data:0863`), this
/// test surfaces the mismatch alongside the reachability failures
/// above so the diagnosis is self-evident in CI output.
#[test]
fn goal_pattern_iri_matches_peak_calling_output_iri() {
    let atom_reg = AtomRegistry::load_from_dir(Path::new(ATOMS_DIR))
        .expect("AtomRegistry must load from config/stage-atoms");
    let atom = atom_reg
        .get("peak_calling")
        .expect("peak_calling atom must be present in registry");
    let output_iri = atom
        .outputs
        .first()
        .expect("peak_calling must have at least one output port")
        .semantic_type
        .stable_id();
    assert_eq!(
        output_iri, "data:1255",
        "peak_calling output IRI drifted from data:1255 (Feature record). \
         If this changed intentionally, update peak_calling_goal()'s \
         edam_data + the matching pattern in modality-keywords.yaml."
    );
}

/// End-to-end: classifying a chip-seq prose with the real
/// `modality-keywords.yaml` must produce a goal whose `edam_data`
/// matches `peak_calling`'s output IRI (`data:1255`, Feature record).
/// Guards against config drift where the YAML's data IRI disagrees
/// with the comment ("EDAM data:3002 = Annotation track").
#[test]
fn classifier_chip_seq_peak_calling_prose_matches_atom_output() {
    use scripps_workflow_core::classify::Classifier;
    let keywords_path = Path::new("../../config/modality-keywords.yaml");
    let classifier = Classifier::load(keywords_path).expect("load modality-keywords.yaml");
    let prose = "Align reads with BWA, call narrow peaks with MACS2 against matched input controls";
    let classification = classifier.classify(prose);
    let goal = classification
        .goal
        .as_ref()
        .expect("classifier must extract a goal from chip-seq peak-calling prose");
    assert_eq!(
        goal.edam_data, "data:1255",
        "chip-seq peak-calling goal pattern emits the wrong edam_data. \
         The correct value matching peak_calling.yaml's output port \
         is data:1255 (Feature record); data:0863 (BAM) does not unify."
    );
    assert_eq!(
        goal.edam_format.as_deref(),
        Some("format:3003"),
        "chip-seq peak-calling goal pattern must emit format:3003 (BED)"
    );
}

/// Production-dispatch path must succeed for chip-seq end-to-end
/// (classifier → composer → DAG with peak_calling). The
/// `compose_with_version_and_modalities_full` call must return a
/// `ValidatedExecutableDag` containing `peak_calling` (not
/// `GoalUnreachable`).
#[test]
fn production_dispatch_reaches_peak_calling_for_chip_seq() {
    use scripps_workflow_core::classify::Classifier;
    use scripps_workflow_core::composer::compose_with_version_and_modalities_full;

    let atom_reg = AtomRegistry::load_from_dir(Path::new(ATOMS_DIR))
        .expect("AtomRegistry must load from config/stage-atoms");
    let archetype_reg = ArchetypeRegistry::load_from_dir(Path::new(ARCHETYPES_DIR))
        .expect("ArchetypeRegistry must load from config/archetypes");
    let keywords_path = Path::new("../../config/modality-keywords.yaml");
    let classifier = Classifier::load(keywords_path).expect("load modality-keywords.yaml");
    let prose =
        "ChIP-seq peak calling for H3K27ac in HEK293 cells; align with BWA-MEM2 and call narrow \
         peaks with MACS2 against matched input controls";
    let classification = classifier.classify(prose);
    let goal = classification
        .goal
        .clone()
        .expect("classifier must produce a goal for chip-seq peak-calling prose");

    let mut modalities: Vec<&str> = vec![classification.modality.as_str()];
    for m in &classification.additional_modalities {
        modalities.push(m.modality.as_str());
    }

    let result = compose_with_version_and_modalities_full(
        &goal,
        "bioinformatics",
        &atom_reg,
        &archetype_reg,
        4,
        &modalities,
        None,
        None,
        None,
    );
    let output = result.expect(
        "v4 production dispatch must succeed for chip-seq peak-calling \
         goal post-Task-F fix",
    );
    let task_ids: BTreeSet<String> = output
        .composition
        .atoms
        .iter()
        .map(|c| c.atom.id.clone())
        .collect();
    assert!(
        task_ids.contains("peak_calling"),
        "peak_calling missing from chip-seq production-dispatch DAG: {task_ids:?}"
    );
}
