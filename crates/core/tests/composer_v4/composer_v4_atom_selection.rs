//! v4 atom selection must not include atoms whose `method_choice`
//! references a missing discovery atom and must not include
//! wide-spectrum agentic / benchmark atoms when the intent declares
//! a modality.
//!
//! Before the fix, the v4 search walked the entire `AtomRegistry` whose
//! ports type-unify with reachable producers. Atoms with a
//! `method_choice.deferred_to` pointing at a missing discovery atom and
//! the LLM benchmark eval atoms (`bio_mystery_query`, `compbio_query`,
//! `sciagent_solution`, `hle_bio_query`, `lab_bench_query`,
//! `biomni_eval1_query`) all output `data:0951`-parented eval scores
//! that match common goal types. Those leaked into the search frontier
//! and surfaced as `MethodChoiceUnresolved` failures in
//! `validate_composition`.
//!
//! After the fix the search filters atoms with unresolvable
//! `method_choice` universally and filters benchmark / agentic atoms
//! when modality is set on the intent — both run in `forward_search`
//! and `backward_search` so the meet-in-middle assembly only sees
//! eligible atoms. All previously-deferred atoms
//! (`differential_transcript_usage`, `isoform_discovery`,
//! `time_series_model_fitting`, `spatial_domain_segmentation`) now have
//! their paired discover counterparts in the catalog, so the
//! `UNRESOLVED_METHOD_CHOICE_ATOMS` guard list is currently empty.

use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::path::Path;

use ecaa_workflow_core::archetype_registry::ArchetypeRegistry;
use ecaa_workflow_core::atom_registry::AtomRegistry;
use ecaa_workflow_core::composer_v4::{plan as v4_plan, PlanningContext};
use ecaa_workflow_core::goal_spec::GoalSpec;
use ecaa_workflow_core::workflow_contracts::data_product::DataProductContract;
use ecaa_workflow_core::workflow_contracts::outcome::ComposeOutcome;
use ecaa_workflow_core::workflow_contracts::workflow_intent::{DesiredOutput, WorkflowIntent};

const ATOMS_DIR: &str = "../../config/stage-atoms";
const ARCHETYPES_DIR: &str = "../../config/archetypes";

/// Agentic / benchmark eval atoms that must not leak into modality-
/// routed scenarios. All six declare `figure_exempt.category =
/// non_plottable` and produce eval-score outputs whose `data:0951`
/// parent terms type-unify with bulk-rnaseq DE / time-series goals.
const AGENTIC_ATOMS: &[&str] = &[
    "bio_mystery_query",
    "compbio_query",
    "sciagent_solution",
    "endpoint_analysis",
    "hle_bio_query",
    "lab_bench_query",
    "biomni_eval1_query",
];

/// Atoms whose `method_choice.deferred_to` references a discovery atom
/// that doesn't exist in the registry. The composer's
/// `validate_composition` rejects such compositions with
/// `MethodChoiceUnresolved`; the v4 search must not propose them.
///
/// This list is populated when a new atom ships before its paired
/// `discover_*` atom is added to `config/stage-atoms/`. Currently
/// empty because all previously-deferred atoms
/// (`differential_transcript_usage`, `isoform_discovery`,
/// `time_series_model_fitting`, `spatial_domain_segmentation`) now have
/// their discover counterparts in the catalog.
const UNRESOLVED_METHOD_CHOICE_ATOMS: &[&str] = &[];

/// Bulk-RNA-seq DE goal mirrored from the parity-corpus fixture.
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

/// Spatial-transcriptomics goal — spatial-domain / SVG discovery on a
/// Visium DLPFC section. Mirrors the Maynard 2021 layer-identification
/// recreation: the modality-specific atoms `spatial_domain_segmentation`
/// and `spatially_variable_genes` must appear, not a generic scRNA
/// clustering pipeline.
fn spatial_transcriptomics_goal() -> GoalSpec {
    GoalSpec {
        edam_data: "data:3917".into(),
        edam_format: Some("format:3475".into()),
        modifiers: BTreeMap::new(),
        source_prose: Some(
            "Spatial transcriptomics Visium DLPFC analysis: segment the tissue \
             into spatial domains and discover spatially variable genes per domain."
                .into(),
        ),
        confidence: 0.9,
    }
}

/// Variant-calling germline goal.
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

/// Run the v4 planner with the given modality + goal, then return the
/// set of atom (task-node) ids in the primary outcome's DAG. Mirrors
/// `composer_v4_no_cycles.rs::run_v4_planner` so the per-Task TDD
/// scaffolds stay close to each other.
fn task_ids_for(modality: &str, goal: &GoalSpec) -> BTreeSet<String> {
    let atom_reg = AtomRegistry::load_from_dir(Path::new(ATOMS_DIR))
        .expect("AtomRegistry must load from config/stage-atoms");
    let archetype_reg = ArchetypeRegistry::load_from_dir(Path::new(ARCHETYPES_DIR))
        .expect("ArchetypeRegistry must load from config/archetypes");
    let intent = WorkflowIntent {
        id: format!("v4_atom_selection_{modality}"),
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

    // Collect the union of atom ids across primary + all alternatives.
    // Filtering must hold whichever alternative the planner returns as
    // primary, so we assert across the entire slate.
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

/// Like `task_ids_for` but returns only the PRIMARY outcome's task ids —
/// i.e. exactly what the package would emit. Used by the spatial
/// recreation test, where "appears somewhere in the alternative slate"
/// is not enough: the emitted DAG itself must carry the spatial atoms.
fn primary_task_ids_for(modality: &str, goal: &GoalSpec) -> BTreeSet<String> {
    let atom_reg = AtomRegistry::load_from_dir(Path::new(ATOMS_DIR))
        .expect("AtomRegistry must load from config/stage-atoms");
    let archetype_reg = ArchetypeRegistry::load_from_dir(Path::new(ARCHETYPES_DIR))
        .expect("ArchetypeRegistry must load from config/archetypes");
    let intent = WorkflowIntent {
        id: format!("v4_primary_{modality}"),
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
    match &result.primary {
        ComposeOutcome::ValidatedExecutableDag { dag, .. }
        | ComposeOutcome::DraftDag { dag, .. }
        | ComposeOutcome::PartialDag { dag, .. } => {
            for n in &dag.nodes {
                ids.insert(n.id.clone());
            }
        }
        _ => {}
    }
    ids
}

/// Agentic / benchmark eval atoms must not leak into a
/// modality-routed bulk-RNA-seq DAG. `bio_mystery_query` /
/// `compbio_query` / `sciagent_solution` etc. all output
/// `data:0951`-parented `eval_score` ports that backward-search
/// would otherwise match against the bulk-rnaseq DE goal.
#[test]
fn v4_excludes_agentic_atoms_from_bulk_rnaseq() {
    let task_ids = task_ids_for("bulk_rnaseq", &bulk_rnaseq_de_goal());
    assert!(
        !task_ids.is_empty(),
        "v4 produced no DAG for bulk-rnaseq — pre-Task-A regression"
    );
    for forbidden in AGENTIC_ATOMS {
        assert!(
            !task_ids.contains(*forbidden),
            "agentic atom {forbidden} leaked into bulk-rnaseq v4 DAG: {task_ids:?}"
        );
    }
}

/// Same check for variant-calling. The germline VCF
/// goal (`data:3498`) shouldn't backward-search into eval-score atoms,
/// but the search-side filter should still hold defensively for any
/// modality-routed scenario.
#[test]
fn v4_excludes_agentic_atoms_from_variant_calling() {
    let task_ids = task_ids_for("variant_calling", &variant_calling_goal());
    assert!(
        !task_ids.is_empty(),
        "v4 produced no DAG for variant-calling — pre-Task-A regression"
    );
    for forbidden in AGENTIC_ATOMS {
        assert!(
            !task_ids.contains(*forbidden),
            "agentic atom {forbidden} leaked into variant-calling v4 DAG: {task_ids:?}"
        );
    }
}

/// Atoms whose `method_choice.deferred_to`
/// references a discovery atom that doesn't exist in the registry must
/// not appear in any v4 DAG. Including such an atom surfaces
/// `MethodChoiceUnresolved` from `validate_composition`. The
/// `UNRESOLVED_METHOD_CHOICE_ATOMS` list is currently empty because
/// every previously-deferred atom now has its paired `discover_*`
/// counterpart in the catalog; the guard remains so new atoms added
/// before their discover counterpart can be caught here.
#[test]
fn v4_skips_atoms_with_unresolved_method_choice() {
    let task_ids = task_ids_for("bulk_rnaseq", &bulk_rnaseq_de_goal());
    assert!(
        !task_ids.is_empty(),
        "v4 produced no DAG for bulk-rnaseq — pre-Task-A regression"
    );
    for forbidden in UNRESOLVED_METHOD_CHOICE_ATOMS {
        assert!(
            !task_ids.contains(*forbidden),
            "atom {forbidden} has unresolvable method_choice; must not appear in v4 DAG: \
             {task_ids:?}"
        );
    }
}

/// Variant-calling regression test for the
/// method-choice filter. This test makes sure the filter does not
/// regress the variant-calling production dispatch path while still
/// excluding any future unresolvable atoms from any scenario's slate.
#[test]
fn v4_skips_atoms_with_unresolved_method_choice_variant_calling() {
    let task_ids = task_ids_for("variant_calling", &variant_calling_goal());
    assert!(
        !task_ids.is_empty(),
        "v4 produced no DAG for variant-calling — pre-Task-A regression"
    );
    for forbidden in UNRESOLVED_METHOD_CHOICE_ATOMS {
        assert!(
            !task_ids.contains(*forbidden),
            "atom {forbidden} has unresolvable method_choice; must not appear in v4 DAG: \
             {task_ids:?}"
        );
    }
}

/// A spatial-transcriptomics workflow must recreate the spatial analysis,
/// not collapse to a generic scRNA clustering pipeline. The
/// modality-specific atoms `spatial_domain_segmentation` (spatial-domain
/// clustering, BANKSY/BayesSpace/GraphST) and `spatially_variable_genes`
/// (Moran's I / SpatialDE SVG discovery) both exist in the catalog with
/// registered renderers; the emitted DAG must include them and their
/// auto-synthesized `discover_spatial_clustering_method` companion.
#[test]
fn v4_spatial_transcriptomics_includes_spatial_atoms() {
    let task_ids = primary_task_ids_for("spatial_transcriptomics", &spatial_transcriptomics_goal());
    assert!(
        !task_ids.is_empty(),
        "v4 produced no DAG for spatial_transcriptomics"
    );
    for required in [
        "spatial_domain_segmentation",
        "spatially_variable_genes",
        "discover_spatial_clustering_method",
    ] {
        assert!(
            task_ids.contains(required),
            "spatial atom {required} missing from spatial_transcriptomics v4 DAG \
             (generic-scRNA collapse regression): {task_ids:?}"
        );
    }
}
